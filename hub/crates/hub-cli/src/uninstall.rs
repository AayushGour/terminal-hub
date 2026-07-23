// `hub uninstall` (Plan 3, Task 7): the exact, safe inverse of `hub install`.
//
// SAFETY MODEL (non-negotiable — this code edits/deletes the user's rc files):
//   * Restore writes the *exact* pre-install backup bytes back, and does so
//     ATOMICALLY (temp file in the same dir, fsync, rename) — a crash or
//     ENOSPC mid-write can never leave a truncated rc file. This mirrors
//     install's `atomic_write`.
//   * When the user edited the rc after install (hash no longer matches) or the
//     backup is gone, we fall back to `snippet::remove_block`, which removes
//     ONLY our marked block and is a guaranteed no-op if the markers are
//     missing/unmatched — so unrelated user config is never touched. This
//     fallback is byte-safe: it only ever rewrites content that is valid
//     UTF-8 AND genuinely contains hub's marker. Non-UTF-8 rc files (where
//     `from_utf8_lossy` would otherwise fabricate a spurious "changed" diff
//     via U+FFFD substitution and trigger a corrupting rewrite) are left
//     completely untouched.
//   * `--dry-run` (`plan_dry_run` + the `run` dry-run branch) performs ZERO
//     filesystem mutation and ZERO state-changing daemon calls.
//   * `created_by_hub` files (e.g. a `.bash_profile` hub itself created) did
//     not exist before install, so an UNTOUCHED one (hash still matches what
//     hub wrote) is deleted outright. If the user has since EDITED it, hub
//     never wholesale-deletes it — that would destroy their added content.
//     Instead only hub's own marked block is surgically removed, and the
//     file is deleted only if that leaves nothing but whitespace behind.
use crate::manifest::{Manifest, TouchedFile};
use crate::{autostart, daemon_client, manifest, paths, snippet};
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    RestoredBackup,
    SurgicallyRemoved,
    Deleted,
    Missing,
    /// `created_by_hub` file was edited by the user since hub created it
    /// (post-install hash mismatch); hub's own block was surgically removed
    /// (if present) but the file itself was left in place because it still
    /// carries user content.
    CreatedFileKeptAfterEdit,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn block_markers(block: &str) -> (&'static str, &'static str) {
    match block {
        "bridge" => (snippet::BRIDGE_BEGIN, snippet::BRIDGE_END),
        _ => (snippet::BEGIN, snippet::END),
    }
}

/// Write `bytes` to `target` atomically: temp file in the SAME directory,
/// fsync, then `rename` into place (atomic on POSIX within a filesystem).
/// Byte-exact — used both for backup restore and surgical cleanup so neither
/// path can ever leave a partially written rc file.
fn atomic_write_bytes(target: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.hubtmp.{}",
        target.file_name().and_then(|s| s.to_str()).unwrap_or("rc"),
        std::process::id()
    ));
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("create temp {}", tmp.display()))?;
        f.write_all(bytes)
            .with_context(|| format!("write temp {}", tmp.display()))?;
        f.flush()?;
        f.sync_all()?;
    }
    // Preserve the target's existing permissions (e.g. a deliberately 0600
    // rc file) across the atomic swap. `File::create` applies the process
    // umask/default mode to the temp file, which would otherwise silently
    // loosen (or tighten) the original file's mode on every restore/cleanup.
    // If the target doesn't exist (a `created_by_hub` file, restored for the
    // first time) there is nothing to preserve, so leave the default mode.
    if let Ok(meta) = fs::metadata(target) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

/// Attempt to surgically remove hub's marked `begin..=end` block from
/// `current` and, if that changes anything, atomically write the result to
/// `path`. Byte-safe and conservative (see the module-level SAFETY MODEL):
///   * `current` must be valid UTF-8 — non-UTF-8 content is never touched,
///     since `String::from_utf8_lossy` would mask/replace invalid bytes and
///     could make an unrelated file look "changed" when it never was.
///   * hub's BEGIN marker must actually be present (`snippet::contains_block`)
///     — a file with no hub block is never rewritten.
///   * `snippet::remove_block` itself is a no-op (returns content unchanged)
///     if BEGIN is found without a matching END, guarding against a
///     corrupted/unmatched marker pair.
/// Returns `Ok(Some(new_bytes))` if the file was rewritten, `Ok(None)` if it
/// was left completely untouched (nothing to remove, or not safely
/// modifiable).
fn surgical_remove_block(
    path: &Path,
    current: &[u8],
    begin: &str,
    end: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    match std::str::from_utf8(current) {
        Ok(current_str) if snippet::contains_block(current_str, begin) => {
            let cleaned = snippet::remove_block(current_str, begin, end);
            if cleaned != current_str {
                atomic_write_bytes(path, cleaned.as_bytes())?;
                Ok(Some(cleaned.into_bytes()))
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

/// Reverse hub's touch of a single rc file.
///   * file gone            → `Missing` (nothing to do).
///   * `created_by_hub`     → delete it (it did not exist pre-install).
///   * untouched + backup   → atomic byte-for-byte restore (`RestoredBackup`).
///   * otherwise            → surgical `remove_block` of only our marked block
///     (`SurgicallyRemoved`); a no-op if the markers are absent, so user edits
///     are preserved verbatim.
pub fn restore_file(t: &TouchedFile) -> anyhow::Result<RestoreOutcome> {
    let path = Path::new(&t.path);
    if !path.exists() {
        return Ok(RestoreOutcome::Missing);
    }
    let current = fs::read(path)?;
    let (begin, end) = block_markers(&t.block);

    if t.created_by_hub {
        if sha256_hex(&current) == t.post_install_sha256 {
            // Untouched since hub created it → nothing of the user's would
            // be lost, so it's safe to delete wholesale.
            fs::remove_file(path)?;
            return Ok(RestoreOutcome::Deleted);
        }
        // Edited since hub created it: NEVER wholesale-delete — that could
        // destroy content the user added on top of hub's bridge block.
        // Surgically remove only hub's own block; only delete the file if
        // doing so leaves nothing but whitespace behind (i.e. genuinely
        // equivalent to the untouched case).
        return match surgical_remove_block(path, &current, begin, end)? {
            Some(new_bytes) if new_bytes.iter().all(u8::is_ascii_whitespace) => {
                fs::remove_file(path)?;
                Ok(RestoreOutcome::Deleted)
            }
            _ => Ok(RestoreOutcome::CreatedFileKeptAfterEdit),
        };
    }

    if sha256_hex(&current) == t.post_install_sha256 {
        // Untouched since install → exact restore from backup (atomic).
        if let Some(backup) = &t.backup {
            let bytes = fs::read(backup)
                .with_context(|| format!("read backup {backup}"))?;
            atomic_write_bytes(path, &bytes)?;
            return Ok(RestoreOutcome::RestoredBackup);
        }
    }
    // User edited it (or backup missing): surgically remove ONLY our marked
    // block via the byte-safe helper. It is a guaranteed no-op (no write at
    // all) if the content isn't valid UTF-8, the marker is absent, or the
    // marker pair is unmatched/corrupted — so we never guess or truncate.
    surgical_remove_block(path, &current, begin, end)?;
    Ok(RestoreOutcome::SurgicallyRemoved)
}

/// Human-readable, read-only preview of everything `run` would touch.
/// Pure: builds strings from the manifest and performs NO filesystem mutation.
pub fn plan_dry_run(home: &Path, m: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for t in &m.entries {
        if t.created_by_hub {
            out.push(format!("delete (hub-created): {}", t.path));
        } else if let Some(b) = &t.backup {
            out.push(format!("restore {} from backup {}", t.path, b));
        } else {
            out.push(format!("clean hub block in {}", t.path));
        }
    }
    if let Some(a) = &m.autostart {
        out.push(format!("remove autostart: {a:?}"));
    }
    out.push(format!("delete tree: {}", paths::hub_dir(home).display()));
    for b in &m.binaries {
        out.push(format!("remove binary: {b}"));
    }
    out
}

pub async fn run(home: &Path, yes: bool, dry_run: bool) -> anyhow::Result<()> {
    let m = manifest::load(&paths::manifest_path(home))?;

    // Warn about live sessions (best-effort; daemon may be down).
    let sock = paths::daemon_sock_path(home);
    let live = daemon_client::list_sessions(&sock).await.unwrap_or_default();

    if dry_run {
        println!("hub uninstall --dry-run (nothing will change):");
        println!("  {} live session(s) would terminate", live.len());
        for line in plan_dry_run(home, &m) {
            println!("  {line}");
        }
        return Ok(());
    }

    if !yes {
        println!(
            "hub uninstall will terminate {} live session(s) and:",
            live.len()
        );
        for line in plan_dry_run(home, &m) {
            println!("  {line}");
        }
        print!("Proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            anyhow::bail!("aborted by user");
        }
    }

    // 1. Kill live sessions, then stop the daemon PROCESS via
    //    ControlMsg::Shutdown. NOTE: relays are NOT killed by daemon
    //    Shutdown (SPOF design -- relays own the ptys and survive daemon
    //    death); the session kills above are what actually tears sessions
    //    down. Autostart removal below is the fallback if the daemon is
    //    already unreachable.
    let _ = daemon_client::shutdown_daemon(&sock).await;
    // 2. Restore rc files.
    for t in &m.entries {
        match restore_file(t) {
            Ok(o) => println!("  {}: {o:?}", t.path),
            Err(e) => eprintln!("  {}: restore failed: {e:#}", t.path),
        }
    }
    // 3. Remove autostart.
    if let Some(a) = &m.autostart {
        let _ = autostart::remove_autostart(a);
    }
    // 4. Delete ~/.hub.
    let _ = fs::remove_dir_all(paths::hub_dir(home));
    // 5. Remove binaries (last; unlinking a running binary is fine on unix).
    for b in &m.binaries {
        let _ = fs::remove_file(b);
    }
    println!("hub uninstalled. Open a new terminal for a clean shell.");
    Ok(())
}
