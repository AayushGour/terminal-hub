use crate::manifest::{Manifest, TouchedFile};
use crate::rcfile::{plan_rc, BridgeKind, Shell};
use crate::{autostart, manifest, paths, snippet};
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

/// Write `content` to `target` atomically: write to a temp file in the SAME
/// directory as `target`, fsync it, then `rename` it into place. On POSIX,
/// `rename` within a filesystem is atomic, so a crash or ENOSPC between the
/// temp write and the rename leaves `target` as either the old complete file
/// or the new complete file — never truncated/partial.
fn atomic_write(target: &Path, content: &str) -> anyhow::Result<()> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    // unique temp name in the same dir (same filesystem => rename is atomic)
    let tmp = dir.join(format!(
        ".{}.hubtmp.{}",
        target.file_name().and_then(|s| s.to_str()).unwrap_or("rc"),
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)
            .with_context(|| format!("create temp {}", tmp.display()))?;
        f.write_all(content.as_bytes())
            .with_context(|| format!("write temp {}", tmp.display()))?;
        f.flush()?;
        f.sync_all()?;
    }
    // Preserve the target's existing permissions (e.g. a deliberately 0600
    // rc file) across the atomic swap. `File::create` applies the process
    // umask/default mode to the temp file, which would otherwise silently
    // loosen (or tighten) the original file's mode on every install. If the
    // target doesn't exist yet (we're creating it), there's nothing to
    // preserve, so leave the default mode.
    if let Ok(meta) = fs::metadata(target) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, target)
        .with_context(|| format!("rename {} -> {}", tmp.display(), target.display()))?;
    Ok(())
}

/// Robustly decide whether a shell rc file contains a *live* (non-commented)
/// statement that sources `~/.bashrc`. Correctness matters here: a false
/// positive (treating a commented-out source as live) skips a needed bridge and
/// silently breaks auto-capture in login shells, so we are deliberately
/// conservative — we only return true when a real `.`/`source` builtin targets
/// a bashrc path. A false negative merely adds a redundant, harmless bridge.
///
/// Handles the common forms:
///   `. ~/.bashrc`, `source ~/.bashrc`,
///   `. "$HOME/.bashrc"`, `source ${HOME}/.bashrc`,
///   `[ -f ~/.bashrc ] && . ~/.bashrc`,
///   `if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi`
/// and IGNORES any line whose first non-space character is `#`.
fn content_sources_bashrc(content: &str) -> bool {
    content.lines().any(line_sources_bashrc)
}

fn line_sources_bashrc(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return false; // whole line commented out → not live.
    }
    // Break the line on shell command separators so a `. <bashrc>` pair is
    // adjacent in the resulting word list even inside `if …; then . …; fi`.
    let normalized = trimmed
        .replace(';', " ; ")
        .replace("&&", " && ")
        .replace("||", " || ");
    let words: Vec<&str> = normalized.split_whitespace().collect();
    for i in 0..words.len() {
        if words[i] == "." || words[i] == "source" {
            if let Some(arg) = words.get(i + 1) {
                if arg_is_bashrc(arg) {
                    return true;
                }
            }
        }
    }
    false
}

fn arg_is_bashrc(arg: &str) -> bool {
    let a = arg.trim_matches(|c| c == '"' || c == '\'');
    matches!(a, "~/.bashrc" | "$HOME/.bashrc" | "${HOME}/.bashrc")
}

pub fn detect_shells(env_shell: Option<&str>, exists: &dyn Fn(&Path) -> bool) -> Vec<Shell> {
    // Install for whichever v1 shells the user plausibly has.
    let home = paths::home_dir();
    let mut out = Vec::new();
    let zsh = env_shell.map(|s| s.contains("zsh")).unwrap_or(false) || exists(&home.join(".zshrc"));
    let bash = env_shell.map(|s| s.contains("bash")).unwrap_or(false)
        || exists(&home.join(".bashrc"))
        || exists(&home.join(".bash_profile"));
    if zsh {
        out.push(Shell::Zsh);
    }
    if bash {
        out.push(Shell::Bash);
    }
    if out.is_empty() {
        // Default to zsh on macOS-like defaults; still safe (creates ~/.zshrc).
        out.push(Shell::Zsh);
    }
    out
}

pub fn create_hub_tree(home: &Path) -> anyhow::Result<()> {
    // backups/ holds verbatim copies of the user's rc files, so every dir in
    // this tree (not just ~/.hub itself) must be locked down to 0700.
    for dir in [
        paths::hub_dir(home),
        paths::sessions_dir(home),
        paths::logs_dir(home),
        paths::backups_dir(home),
    ] {
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 0700 {}", dir.display()))?;
    }
    Ok(())
}

fn backup_file(home: &Path, file: &Path) -> anyhow::Result<String> {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let base = file.file_name().and_then(|s| s.to_str()).unwrap_or("rcfile");
    let dst = paths::backups_dir(home).join(format!("{base}.{ts}.bak"));
    fs::copy(file, &dst)
        .with_context(|| format!("backup {} -> {}", file.display(), dst.display()))?;
    Ok(dst.display().to_string())
}

/// Append a marked block (with a single blank separator) if not already present.
/// Returns Some(TouchedFile) if it changed anything, None if already present.
///
/// SAFETY: The pre-existing bytes are backed up *before* the file is rewritten,
/// and the block is only ever appended — unrelated content is preserved verbatim.
/// `contains_block` makes re-runs a no-op, so no double-inject is possible.
fn ensure_block(
    home: &Path,
    file: &Path,
    begin: &str,
    block: &str,
    block_name: &str,
    create_if_missing_content: Option<&str>,
) -> anyhow::Result<Option<TouchedFile>> {
    let existed = file.exists();
    let current = if existed {
        fs::read_to_string(file)?
    } else {
        String::new()
    };

    if snippet::contains_block(&current, begin) {
        return Ok(None); // idempotent: no double-inject.
    }

    let backup = if existed {
        Some(backup_file(home, file)?)
    } else {
        None
    };

    let mut new_content = String::new();
    if let (false, Some(seed)) = (existed, create_if_missing_content) {
        new_content.push_str(seed);
    } else {
        new_content.push_str(&current);
    }
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(block.trim_end());
    new_content.push('\n');

    atomic_write(file, &new_content)?;

    Ok(Some(TouchedFile {
        path: file.display().to_string(),
        backup,
        created_by_hub: !existed,
        post_install_sha256: sha256_hex(new_content.as_bytes()),
        block: block_name.to_string(),
    }))
}

/// CreateProfile branch: bash has no `~/.bash_profile`/`~/.bash_login`, so we
/// create `~/.bash_profile`. Because bash stops reading `~/.profile` once this
/// file exists, we source `~/.profile` explicitly to preserve prior behavior.
/// We add an explicit `. ~/.bashrc` ONLY when `~/.profile` doesn't already bring
/// it in — otherwise we'd double-source `~/.bashrc`. The block is marker-guarded
/// so re-runs are a no-op (idempotent) and uninstall can reverse it.
fn create_bash_profile(home: &Path, file: &Path) -> anyhow::Result<Option<TouchedFile>> {
    let existed = file.exists();
    let current = if existed {
        fs::read_to_string(file)?
    } else {
        String::new()
    };
    if snippet::contains_block(&current, snippet::BRIDGE_BEGIN) {
        return Ok(None); // idempotent.
    }

    let profile_sources_bashrc = fs::read_to_string(home.join(".profile"))
        .map(|c| content_sources_bashrc(&c))
        .unwrap_or(false);

    let backup = if existed {
        Some(backup_file(home, file)?)
    } else {
        None
    };

    let mut content = String::new();
    if existed {
        content.push_str(&current);
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push('\n');
    }
    content.push_str(snippet::BRIDGE_BEGIN);
    content.push('\n');
    content.push_str("# Created by hub. bash ignores ~/.profile once ~/.bash_profile\n");
    content.push_str("# exists, so source it here to preserve prior login-shell behavior,\n");
    content.push_str("# then ensure ~/.bashrc (where hub lives) is loaded.\n");
    content.push_str("if [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\"; fi\n");
    if !profile_sources_bashrc {
        content.push_str("if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n");
    }
    content.push_str(snippet::BRIDGE_END);
    content.push('\n');

    atomic_write(file, &content)?;

    Ok(Some(TouchedFile {
        path: file.display().to_string(),
        backup,
        created_by_hub: !existed,
        post_install_sha256: sha256_hex(content.as_bytes()),
        block: "bridge".to_string(),
    }))
}

/// Copy one executable from `src` to `dst` atomically: write to a temp file in
/// the SAME directory as `dst`, fsync, chmod 0755, then `rename` into place.
/// `rename` within a filesystem is atomic on POSIX, so a re-copy over an
/// already-installed (possibly running) binary either fully succeeds or leaves
/// the previous file intact — never a truncated/half-written executable.
/// Unlinking the old inode is safe even while it is being executed on unix.
fn atomic_copy_exec(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let dir = dst.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(
        ".{}.hubtmp.{}",
        dst.file_name().and_then(|s| s.to_str()).unwrap_or("bin"),
        std::process::id()
    ));
    let bytes = fs::read(src).with_context(|| format!("read {}", src.display()))?;
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("create temp {}", tmp.display()))?;
        f.write_all(&bytes)
            .with_context(|| format!("write temp {}", tmp.display()))?;
        f.flush()?;
        f.sync_all()?;
    }
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
        .with_context(|| format!("chmod 0755 {}", tmp.display()))?;
    fs::rename(&tmp, dst)
        .with_context(|| format!("rename {} -> {}", tmp.display(), dst.display()))?;
    Ok(())
}

/// Self-contained install: copy `hub`, `hub-daemon`, `hub-relay` from `bin_src`
/// into `<hub_home>/bin` (created 0700) so the install keeps working after the
/// installing app bundle is deleted. Each binary is written atomically at mode
/// 0755, and its final path is recorded in the manifest `binaries` field (once
/// — re-runs don't duplicate entries) with the manifest persisted after each so
/// a crash mid-copy leaves it truthful. `hub uninstall`'s `remove_dir_all` of
/// `~/.hub` is what actually removes the binaries; recording keeps `--dry-run`
/// accurate. Returns the copied `hub-daemon` path so autostart can point at the
/// self-contained copy rather than one beside the (deletable) app bundle.
pub fn copy_binaries(
    home: &Path,
    bin_src: &Path,
    m: &mut Manifest,
) -> anyhow::Result<std::path::PathBuf> {
    let bin_dir = paths::bin_dir(home);
    fs::create_dir_all(&bin_dir).with_context(|| format!("mkdir {}", bin_dir.display()))?;
    fs::set_permissions(&bin_dir, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 0700 {}", bin_dir.display()))?;
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let src = bin_src.join(name);
        let dst = bin_dir.join(name);
        atomic_copy_exec(&src, &dst)
            .with_context(|| format!("copy {} -> {}", src.display(), dst.display()))?;
        let dst_str = dst.display().to_string();
        // Idempotent: a re-run re-copies atomically but must not append a
        // duplicate manifest entry.
        if !m.binaries.contains(&dst_str) {
            m.binaries.push(dst_str);
            // Persist after EACH recorded copy so a crash mid-copy still leaves
            // the manifest truthful about what's already on disk (mirrors
            // `inject_all`'s per-edit persistence).
            persist(home, m)?;
        }
    }
    Ok(bin_dir.join("hub-daemon"))
}

/// Copy a built `.app` bundle into `/Applications/<same-basename>`, replacing
/// any existing bundle at that destination. Uses `ditto` rather than a manual
/// walk or `cp -R`: it's Apple's own tool for copying bundles and is the only
/// one guaranteed to preserve everything a signed `.app` needs verbatim
/// (resource forks, xattrs incl. the code-signature-relevant ones, symlinks)
/// — a naive recursive copy can silently produce a bundle macOS refuses to
/// launch (Gatekeeper) or treats as damaged.
/// Returns the destination path on success.
#[cfg(target_os = "macos")]
pub fn install_app_bundle(src: &Path) -> anyhow::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("app bundle path has no file name: {}", src.display()))?;
    let dest = Path::new("/Applications").join(name);
    // `ditto` overwrites an existing destination bundle in place (unlike
    // `cp -R`, which would nest src inside an existing same-named dir).
    let status = std::process::Command::new("ditto")
        .arg(src)
        .arg(&dest)
        .status()
        .with_context(|| format!("run ditto {} -> {}", src.display(), dest.display()))?;
    if !status.success() {
        anyhow::bail!("ditto {} -> {} failed: {status}", src.display(), dest.display());
    }
    Ok(dest)
}

#[cfg(not(target_os = "macos"))]
pub fn install_app_bundle(_src: &Path) -> anyhow::Result<PathBuf> {
    anyhow::bail!("--app-bundle is only supported on macOS")
}

/// Persist `m` to `home`'s manifest path immediately. Called after every
/// single rc-file edit inside `inject_all` (and again after autostart is
/// configured in `run`) so a crash mid-install never leaves an edited rc
/// file / backup on disk without a corresponding on-disk manifest entry.
/// `hub uninstall` reads this file, so the invariant we're protecting is:
/// no rc file is ever edited without its manifest entry being durable
/// *before* the next edit is attempted.
fn persist(home: &Path, m: &Manifest) -> anyhow::Result<()> {
    manifest::save(&paths::manifest_path(home), m)
}

pub fn inject_all(home: &Path, shells: &[Shell], m: &mut Manifest) -> anyhow::Result<()> {
    let exists = |p: &Path| p.exists();
    let sources_bashrc = |p: &Path| {
        fs::read_to_string(p)
            .map(|c| content_sources_bashrc(&c))
            .unwrap_or(false)
    };

    for &shell in shells {
        let plan = plan_rc(shell, home, &exists, &sources_bashrc);
        let block = match shell {
            Shell::Zsh => snippet::ZSH,
            Shell::Bash => snippet::BASH,
        };
        if let Some(t) = ensure_block(home, &plan.primary, snippet::BEGIN, block, "snippet", None)? {
            m.entries.push(t);
            // Persist BEFORE moving on to the next edit: if we crash right
            // after this, the manifest already knows about the backup + the
            // block we just appended, so `hub uninstall` can still find and
            // revert it.
            persist(home, m)?;
        }
        if let Some((bridge_file, kind)) = plan.bridge {
            let touched = match kind {
                BridgeKind::AppendSourceBashrc => ensure_block(
                    home,
                    &bridge_file,
                    snippet::BRIDGE_BEGIN,
                    snippet::BASH_PROFILE_BRIDGE,
                    "bridge",
                    None,
                )?,
                BridgeKind::CreateProfile => create_bash_profile(home, &bridge_file)?,
            };
            if let Some(t) = touched {
                m.entries.push(t);
                persist(home, m)?;
            }
        }
    }
    Ok(())
}

pub fn run(
    home: &Path,
    yes: bool,
    bin_src: Option<&Path>,
    app_bundle: Option<&Path>,
) -> anyhow::Result<()> {
    let env_shell = std::env::var("SHELL").ok();
    let exists = |p: &Path| p.exists();
    let shells = detect_shells(env_shell.as_deref(), &exists);

    if !yes {
        println!("hub install will:");
        println!("  - create {} (0700)", paths::hub_dir(home).display());
        if let Some(src) = bin_src {
            println!(
                "  - copy hub, hub-daemon, hub-relay from {} into {} (0755)",
                src.display(),
                paths::bin_dir(home).display()
            );
        }
        if let Some(src) = app_bundle {
            if let Some(name) = src.file_name() {
                println!(
                    "  - copy {} into /Applications/{}",
                    src.display(),
                    Path::new(name).display()
                );
            }
        }
        for s in &shells {
            println!("  - inject a guarded snippet for {s:?} (backing up your rc first)");
        }
        println!("  - set up daemon autostart ({})", autostart::kind_label());
        print!("Proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            anyhow::bail!("aborted by user");
        }
    }

    create_hub_tree(home)?;
    let mut m = manifest::load(&paths::manifest_path(home))?;
    // `inject_all` persists the manifest to disk after EVERY rc-file edit it
    // makes (see `persist` above), so by the time it returns, every backup +
    // guarded block it wrote is already durably recorded — a crash here (or
    // anywhere below) leaves an uninstall-recoverable state.
    inject_all(home, &shells, &mut m)?;

    // Self-contained mode: copy the three binaries into `~/.hub/bin` and point
    // autostart at the COPIED `hub-daemon` so it keeps launching after the
    // installing app bundle is deleted. Without `--bin-src`, behavior is exactly
    // as before: `hub` is assumed on PATH and autostart targets the sibling
    // `hub-daemon` next to the currently-running `hub` (assumption A5).
    let daemon = match bin_src {
        Some(src) => Some(copy_binaries(home, src, &mut m)?),
        None => paths::locate_sibling("hub-daemon"),
    };

    // Autostart (Task 6).
    if let Some(daemon) = daemon {
        match autostart::install_autostart(&daemon, home) {
            Ok(entry) => {
                m.autostart = Some(entry);
                // Persist immediately so the autostart entry is recorded on
                // disk before we print success / return.
                manifest::save(&paths::manifest_path(home), &m)?;
            }
            Err(e) => eprintln!("hub: autostart setup skipped: {e:#}"),
        }
    } else {
        eprintln!("hub: hub-daemon not found next to `hub`; autostart skipped");
    }

    if let Some(src) = app_bundle {
        match install_app_bundle(src) {
            Ok(dest) => {
                m.app_bundle = Some(dest.display().to_string());
                manifest::save(&paths::manifest_path(home), &m)?;
                println!("  - installed app: {}", dest.display());
            }
            Err(e) => eprintln!("hub: app bundle install skipped: {e:#}"),
        }
    }

    println!("hub installed. Open a new terminal to start capturing sessions.");
    println!("Bypass anytime with HUB_DISABLE=1; remove with `hub uninstall`.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_real_source_lines() {
        assert!(content_sources_bashrc(". ~/.bashrc\n"));
        assert!(content_sources_bashrc("source ~/.bashrc\n"));
        assert!(content_sources_bashrc(". \"$HOME/.bashrc\"\n"));
        assert!(content_sources_bashrc("source ${HOME}/.bashrc\n"));
        assert!(content_sources_bashrc("[ -f ~/.bashrc ] && . ~/.bashrc\n"));
        assert!(content_sources_bashrc(
            "if [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n"
        ));
        assert!(content_sources_bashrc("export X=1\n# noise\n. ~/.bashrc\n"));
    }

    #[test]
    fn ignores_commented_and_unrelated_lines() {
        // Commented-out source must NOT count as live (false positive = skipped bridge).
        assert!(!content_sources_bashrc("# . ~/.bashrc\n"));
        assert!(!content_sources_bashrc("    #source ~/.bashrc\n"));
        assert!(!content_sources_bashrc("#if [ -f ~/.bashrc ]; then . ~/.bashrc; fi\n"));
        // A mere existence test with no source is not a live source.
        assert!(!content_sources_bashrc("[ -f ~/.bashrc ]\n"));
        // Different file, not ~/.bashrc.
        assert!(!content_sources_bashrc(". ~/.bashrc.local\n"));
        assert!(!content_sources_bashrc("export PATH=$PATH\n"));
        assert!(!content_sources_bashrc(""));
    }
}
