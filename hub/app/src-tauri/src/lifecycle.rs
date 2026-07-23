// App-lifecycle install/uninstall wiring (feat: app-lifecycle).
//
// This module is the GUI's thin adapter over the ALREADY-TESTED `hub` CLI
// (`hub install --yes --bin-src <DIR>` / `hub uninstall --yes`). It does NOT
// reimplement any rc-file editing, daemon autostart, or manifest logic — every
// filesystem mutation is delegated to the bundled/installed `hub` binary, which
// owns the safety model (atomic writes, byte-for-byte backups, marker-guarded
// blocks). We only:
//   * stage the three bundled binaries out of the (possibly read-only) app
//     bundle into a temp dir with the executable bit set, so the bundled `hub`
//     can actually be run and `--bin-src`'d into `~/.hub/bin`;
//   * shell out to `hub install` / `hub uninstall`, capturing status + stderr
//     and returning a CLEAN error string (never a panic, never any token / pty
//     bytes / environment — those never appear in this path anyway);
//   * on uninstall, self-delete the running `.app` bundle (guarded so we only
//     ever remove a path that genuinely ends in `.app`) and quit.
//
// The pure helpers (`self_delete_command`, `is_safe_app_bundle`,
// `app_bundle_from_exe`, `run_hub_install`, `run_hub_uninstall`) are Tauri-free
// so they can be unit/integration-tested against a throwaway `$HOME` and a fake
// bundled-bin dir without a Tauri runtime and WITHOUT ever trashing a real app
// bundle (the self-delete is gated behind the guard + only fired from the Tauri
// command, never from the tested `run_hub_uninstall`).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The three binaries the install ships / consumes, in the order the CLI's own
/// `copy_binaries` expects them to exist under `--bin-src`.
pub const BIN_NAMES: [&str; 3] = ["hub", "hub-daemon", "hub-relay"];

/// `~/.hub/bin/hub` — the self-contained copy `hub install --bin-src` drops.
/// Preferred over the bundled copy for uninstall so we run the exact binary
/// that recorded the manifest (and so uninstall keeps working even if the app
/// bundle is already gone).
pub fn installed_hub() -> PathBuf {
    crate::hub_home().join("bin").join("hub")
}

/// True iff hub looks installed: the install manifest exists, or the
/// self-contained `hub` binary is present under `~/.hub/bin`. Keyed off the
/// same `hub_home()` (HUB_DIR-aware) the rest of the app uses, so it agrees
/// with what `hub install` wrote. Used to decide whether to prompt on first
/// launch and which lifecycle affordance Settings shows.
pub fn is_installed() -> bool {
    let home = crate::hub_home();
    home.join("install-manifest.json").exists() || installed_hub().exists()
}

/// chmod a file to 0755. Best-effort surface: returns a clean error string on
/// failure so the caller can bubble it up rather than panic.
fn chmod_0755(p: &Path) -> Result<(), String> {
    fs::set_permissions(p, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("chmod 0755 {}: {e}", p.display()))
}

/// Copy the three bundled binaries out of `src` (typically the read-only app
/// bundle's `Contents/Resources/binaries`) into a fresh temp dir and mark them
/// 0755, returning that temp dir. Resources shipped in a `.app` frequently lose
/// the executable bit (and the bundle dir may not be writable to chmod in
/// place), so staging to a writable temp dir is the robust way to end up with a
/// runnable `hub` we can also point `--bin-src` at.
pub fn stage_binaries(src: &Path) -> Result<PathBuf, String> {
    let uniq = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("hub-stage-{}-{}", std::process::id(), uniq));
    fs::create_dir_all(&dir).map_err(|e| format!("create staging dir {}: {e}", dir.display()))?;
    for name in BIN_NAMES {
        let s = src.join(name);
        let d = dir.join(name);
        fs::copy(&s, &d).map_err(|e| format!("stage {}: {e}", s.display()))?;
        chmod_0755(&d)?;
    }
    Ok(dir)
}

/// Keep an error message to its first few lines / a sane length so a clean,
/// UI-friendly string is surfaced (never a wall of output, never anything
/// sensitive — the install/uninstall paths don't print tokens or pty bytes).
fn first_lines(s: &str) -> String {
    let joined = s.lines().take(6).collect::<Vec<_>>().join("; ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        "no error output".to_string()
    } else if trimmed.chars().count() > 500 {
        // Char-safe truncation: `trimmed` may contain multibyte UTF-8
        // characters, so slicing by BYTE index (e.g. `&trimmed[..500]`) can
        // panic with "byte index 500 is not a char boundary" if byte 500
        // lands mid-character. Collecting the first 500 CHARS instead can
        // never panic, regardless of the byte width of what precedes it.
        let head: String = trimmed.chars().take(500).collect();
        format!("{head}…")
    } else {
        trimmed.to_string()
    }
}

/// Run `<bin_dir>/hub install --yes --bin-src <bin_dir>`. `bin_dir` must already
/// hold the three executables at 0755 (see `stage_binaries`). No `HUB_DIR`
/// override is applied — this installs into the real `~/.hub`. Returns a clean
/// error (status + first stderr lines) on any failure.
pub fn run_hub_install(bin_dir: &Path) -> Result<(), String> {
    let hub = bin_dir.join("hub");
    let out = Command::new(&hub)
        .arg("install")
        .arg("--yes")
        .arg("--bin-src")
        .arg(bin_dir)
        .output()
        .map_err(|e| format!("could not run `{}`: {e}", hub.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "hub install failed ({}): {}",
            out.status,
            first_lines(&String::from_utf8_lossy(&out.stderr))
        ))
    }
}

/// Run `<hub_bin> uninstall --yes`. `hub_bin` is normally the installed
/// `~/.hub/bin/hub`, falling back to the bundled copy. NOTE: this performs the
/// reversible teardown ONLY — it never self-deletes the app bundle, so it is
/// safe to exercise in tests against a throwaway `$HOME`.
pub fn run_hub_uninstall(hub_bin: &Path) -> Result<(), String> {
    let out = Command::new(hub_bin)
        .arg("uninstall")
        .arg("--yes")
        .output()
        .map_err(|e| format!("could not run `{}`: {e}", hub_bin.display()))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "hub uninstall failed ({}): {}",
            out.status,
            first_lines(&String::from_utf8_lossy(&out.stderr))
        ))
    }
}

/// GUARD: a path is a safe self-delete target only if its final component ends
/// in `.app` AND it sits at least a couple of levels below the filesystem root
/// (so we can never be tricked into `rm -rf /` or `rm -rf /.app`). Everything
/// downstream of the self-delete path funnels through this check.
pub fn is_safe_app_bundle(p: &Path) -> bool {
    let ends_in_app = p
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.ends_with(".app") && s.len() > ".app".len())
        .unwrap_or(false);
    // e.g. `/Applications/Foo.app` → 3 components ("/", "Applications", "Foo.app").
    ends_in_app && p.components().count() >= 3
}

/// Resolve the running `.app` bundle from this app's executable path:
/// `<Bundle>.app/Contents/MacOS/<binary>` → strip 3 path components →
/// `<Bundle>.app`. Returns `None` (never panics) if the result isn't a safe
/// `.app` bundle — e.g. a `cargo run` / dev-tree binary that lives nowhere near
/// a bundle — so the caller simply skips self-delete rather than removing
/// something arbitrary.
pub fn app_bundle_from_exe(exe: &Path) -> Option<PathBuf> {
    let bundle = exe.parent()?.parent()?.parent()?;
    if is_safe_app_bundle(bundle) {
        Some(bundle.to_path_buf())
    } else {
        None
    }
}

/// Build the DETACHED self-delete command as `(program, argv)`, or `None` if
/// `app` fails the `.app` guard. The bundle path is passed as a positional
/// argument (`$1`) rather than interpolated into the script text, so no shell
/// quoting of the path is required (a bundle path with spaces/quotes can't
/// break out). The script waits ~2s (letting the app fully exit), tries to move
/// the bundle to the Trash via Finder, and falls back to `rm -rf` if that
/// fails. Pure, so the guard + shape are unit-testable without executing it.
pub fn self_delete_command(app: &Path) -> Option<(String, Vec<String>)> {
    if !is_safe_app_bundle(app) {
        return None;
    }
    // `$1` is the bundle path; `$0` is a throwaway "sh".
    let script = r#"sleep 2; osascript -e "tell application \"Finder\" to delete POSIX file \"$1\"" >/dev/null 2>&1 || rm -rf "$1""#;
    Some((
        "sh".to_string(),
        vec![
            "-c".to_string(),
            script.to_string(),
            "sh".to_string(),
            app.display().to_string(),
        ],
    ))
}

/// Spawn the guarded, detached self-delete. Returns `Err` (without spawning
/// anything) if `app` is not a safe `.app` bundle. The child is put in its own
/// process group with stdio detached so it outlives the exiting app and is
/// never tied to a controlling terminal.
pub fn spawn_self_delete(app: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let (prog, args) =
        self_delete_command(app).ok_or_else(|| "refusing to self-delete a non-.app path".to_string())?;
    Command::new(prog)
        .args(args)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("could not spawn self-delete: {e}"))
}

// ---------------------------------------------------------------------------
// Tauri command layer (thin: resolves the bundle resource dir / current exe,
// then delegates to the pure helpers above).
// ---------------------------------------------------------------------------

use tauri::Manager;

/// The bundled `binaries/` dir inside `Contents/Resources` (declared in
/// `tauri.conf.json` `bundle.resources`).
fn bundled_bin_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let res = app
        .path()
        .resource_dir()
        .map_err(|e| format!("could not resolve app resource dir: {e}"))?;
    Ok(res.join("binaries"))
}

/// True if hub's install manifest / self-contained binary is present. Cheap;
/// used by the frontend to decide whether to show the first-run consent dialog.
#[tauri::command]
pub fn hub_is_installed() -> bool {
    is_installed()
}

/// One-time, consent-gated install. Stages the bundled binaries to a writable
/// temp dir (chmod 0755) and runs the bundled `hub install --yes --bin-src`.
/// Real `~/.hub` — no `HUB_DIR` override. Clean error string on failure.
#[tauri::command]
pub fn hub_do_install(app: tauri::AppHandle) -> Result<(), String> {
    let bundled = bundled_bin_dir(&app)?;
    let staged = stage_binaries(&bundled)?;
    let result = run_hub_install(&staged);
    // Best-effort cleanup of the staging dir regardless of outcome (the install
    // already copied the bytes into ~/.hub/bin, so the temp copies are spent).
    let _ = fs::remove_dir_all(&staged);
    result
}

/// Revert everything (`hub uninstall --yes`) then remove this app bundle and
/// quit. Prefers the installed `~/.hub/bin/hub`, falling back to the bundled
/// copy. On success, spawns a guarded detached self-delete (Finder → Trash,
/// falling back to `rm -rf`, only ever targeting a real `.app`) and exits.
#[tauri::command]
pub fn hub_do_uninstall(app: tauri::AppHandle) -> Result<(), String> {
    let hub_bin = if installed_hub().exists() {
        installed_hub()
    } else {
        bundled_bin_dir(&app)?.join("hub")
    };
    run_hub_uninstall(&hub_bin)?;

    // Self-delete the running bundle (guarded) then quit. A dev-tree / non-.app
    // binary resolves to `None` and is simply skipped — we still quit so the
    // "uninstalled" state is honored.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bundle) = app_bundle_from_exe(&exe) {
            let _ = spawn_self_delete(&bundle);
        }
    }
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_bundle_resolves_from_macos_exe_path() {
        let exe = Path::new("/Applications/hub.app/Contents/MacOS/hub");
        assert_eq!(
            app_bundle_from_exe(exe),
            Some(PathBuf::from("/Applications/hub.app"))
        );
    }

    #[test]
    fn app_bundle_from_non_bundle_exe_is_none() {
        // A dev-tree / PATH binary is nowhere near a `.app`; must not resolve to
        // some arbitrary ancestor dir that could then be deleted.
        assert_eq!(app_bundle_from_exe(Path::new("/usr/local/bin/hub")), None);
        assert_eq!(
            app_bundle_from_exe(Path::new("/Users/me/proj/target/debug/hub-app")),
            None
        );
    }

    #[test]
    fn safe_app_bundle_guard_accepts_only_dot_app_paths() {
        assert!(is_safe_app_bundle(Path::new("/Applications/hub.app")));
        assert!(is_safe_app_bundle(Path::new("/Users/me/Applications/My Hub.app")));
        // Not a bundle:
        assert!(!is_safe_app_bundle(Path::new("/Users/me/Desktop/notanapp")));
        assert!(!is_safe_app_bundle(Path::new("/Users/me/hub.app.bak")));
        // Guard against root-ish / degenerate targets:
        assert!(!is_safe_app_bundle(Path::new("/")));
        assert!(!is_safe_app_bundle(Path::new("/.app")));
        assert!(!is_safe_app_bundle(Path::new(".app")));
    }

    #[test]
    fn self_delete_command_is_guarded_and_well_formed() {
        // Guard: a non-.app path yields NO command at all.
        assert!(self_delete_command(Path::new("/Users/me/Desktop/junk")).is_none());
        assert!(self_delete_command(Path::new("/")).is_none());

        // A real .app yields `sh -c <script> sh <APP>`; the path rides as $1
        // (last argv), and the script trashes via Finder with an rm -rf
        // fallback after a delay.
        let (prog, args) = self_delete_command(Path::new("/Applications/hub.app")).unwrap();
        assert_eq!(prog, "sh");
        assert_eq!(args[0], "-c");
        assert_eq!(args.last().unwrap(), "/Applications/hub.app");
        let script = &args[1];
        assert!(script.contains("sleep 2"), "waits for the app to exit");
        assert!(script.contains("Finder"), "moves to Trash via Finder");
        assert!(script.contains("rm -rf \"$1\""), "falls back to rm -rf of $1");
        assert!(
            !script.contains("/Applications/hub.app"),
            "path must not be interpolated into the script text (passed as $1)"
        );
    }

    #[test]
    fn spawn_self_delete_refuses_non_app_path() {
        // Belt-and-suspenders: even the spawn entrypoint refuses a non-.app
        // path, and does so WITHOUT spawning anything.
        let err = spawn_self_delete(Path::new("/tmp/some-dir")).unwrap_err();
        assert!(err.contains("refusing"));
    }

    #[test]
    fn stage_binaries_copies_and_marks_executable() {
        let src = tempfile::tempdir().unwrap();
        for name in BIN_NAMES {
            fs::write(src.path().join(name), b"#!/bin/sh\ntrue\n").unwrap();
            // Simulate a resource that lost its +x bit inside the bundle.
            fs::set_permissions(src.path().join(name), fs::Permissions::from_mode(0o644)).unwrap();
        }
        let staged = stage_binaries(src.path()).unwrap();
        for name in BIN_NAMES {
            let p = staged.join(name);
            assert!(p.is_file(), "{name} staged");
            let mode = fs::metadata(&p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o755, "{name} staged with +x");
        }
        let _ = fs::remove_dir_all(&staged);
    }

    #[test]
    fn first_lines_handles_multibyte_over_limit() {
        // Regression test: `first_lines` used to slice the trimmed string by
        // BYTE index (`&trimmed[..500]`), which panics with "byte index 500
        // is not a char boundary" whenever byte 500 doesn't line up with a
        // UTF-8 char boundary. Build a >500-CHAR string made entirely of a
        // multibyte char ("é", 2 bytes each) -- the worst case for a naive
        // byte slice, since every byte offset sits inside some multibyte
        // sequence -- and assert this returns cleanly (no panic) with a sane,
        // bounded length instead of aborting the process.
        let s: String = "é".repeat(600);
        assert_eq!(s.chars().count(), 600);

        let out = first_lines(&s);

        // Truncated to the first 500 CHARS plus the trailing ellipsis marker.
        assert_eq!(out.chars().count(), 501, "capped to ~500 chars + ellipsis");
        assert!(out.ends_with('…'), "truncated output is marked with an ellipsis");
    }
}
