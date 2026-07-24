// `hub update`: swap an EXISTING install's binaries (and optionally the
// `.app` bundle) for a freshly built copy, in place, WITHOUT disrupting any
// of the user's currently-open/live terminal sessions.
//
// SAFETY MODEL (non-negotiable -- this restarts the daemon process):
//   * Relays own the ptys and are independent, SPOF-surviving processes (see
//     `hub-daemon/src/server.rs`'s `Shutdown` handler) -- stopping the OLD
//     daemon process via `daemon_client::shutdown_daemon` never touches them,
//     exactly like `hub uninstall` already relies on.
//   * The daemon's OWN startup (`hub-daemon/src/server.rs::run`, BEFORE it
//     even binds the control socket) scans `~/.hub/sessions/*.json` and
//     re-dials every still-alive relay's per-session socket to re-register it
//     (`adopt_relay`). So a brand-new daemon process re-discovers every live
//     session with zero involvement from the session itself. That mechanism
//     (already proven for a bare daemon crash/restart) is the entire reason
//     "stop old process, swap binary, start new process" is safe here: by the
//     time the new daemon's socket is reachable again, reconciliation has
//     already run and every live session is back in its registry.
//   * A brief window where the daemon is unreachable is expected and
//     harmless -- viewers (GUI tiles, `hub attach`) may see a transient
//     connection failure, which is exactly the ghost/orphan-recoverable state
//     `app/src-tauri/src/reconcile.rs` already exists to paper over. The pty
//     and the shell behind it are never touched.
//   * Binaries are swapped via `install::copy_binaries` (temp file + atomic
//     rename per binary -- a process still holding the old inode open keeps
//     running against it unaffected), and the manifest is persisted
//     immediately after every mutating step, exactly like `install.rs` /
//     `uninstall.rs`, so a crash mid-update leaves a recoverable, truthful
//     state on disk.
use crate::manifest::Manifest;
use crate::{autostart, daemon_client, install, manifest, paths};
use anyhow::Context;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Poll granularity used while waiting for the daemon socket to go down /
/// come back up around the restart. Deliberately short: both transitions are
/// sub-second in practice (see module doc), so fine-grained polling keeps
/// `hub update` snappy without ever busy-spinning.
const DAEMON_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Bound on total poll time per direction (down, then up): 100 * 20ms = 2s.
/// Generous relative to the sub-second transitions actually observed, but
/// still a hard bound so `hub update` can never hang forever on a daemon that
/// never comes back.
const DAEMON_POLL_ITERS: u32 = 100;

/// A connectable unix socket == a live daemon on the other end (same litmus
/// test `attach::gather_inputs`/`status::run` already use elsewhere).
fn sock_reachable(sock: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(sock).is_ok()
}

/// Bounded poll for the OLD daemon process to actually stop answering its
/// socket after `Shutdown` -- NOT a fixed sleep, since the actual time varies
/// with how busy the machine is (see `DAEMON_POLL_ITERS` doc).
async fn wait_daemon_down(sock: &Path) {
    for _ in 0..DAEMON_POLL_ITERS {
        if !sock_reachable(sock) {
            return;
        }
        tokio::time::sleep(DAEMON_POLL_INTERVAL).await;
    }
}

/// Bounded poll for the NEW daemon process to start answering its socket.
/// Returns whether it came up within budget; a `false` is surfaced as a
/// warning, not a hard failure, since live sessions are unaffected either way
/// (see module doc) -- only daemon reachability lags.
async fn wait_daemon_up(sock: &Path) -> bool {
    for _ in 0..DAEMON_POLL_ITERS {
        if sock_reachable(sock) {
            return true;
        }
        tokio::time::sleep(DAEMON_POLL_INTERVAL).await;
    }
    false
}

/// Spawn `daemon_bin` as a fully detached background process: stdio pointed
/// at `/dev/null` and `setsid()`'d into its own session so closing the
/// terminal `hub update` was run from can never take the daemon down with it
/// via SIGHUP. Unlike `hub-relay`'s `--detach` (`hub-relay/src/detach.rs`)
/// this does NOT double-fork -- nothing here ever `wait()`s on this child, so
/// there is no zombie/reaping concern to guard against; once detached it is
/// simply orphaned to init/launchd, which reaps it on exit.
fn spawn_daemon_detached(daemon_bin: &Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(daemon_bin);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: `setsid()` is async-signal-safe and is the only call made in
    // this pre-exec closure (post-fork, pre-exec, single-threaded child), as
    // required by `Command::pre_exec`'s contract.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .with_context(|| format!("spawn {}", daemon_bin.display()))?;
    Ok(())
}

/// Locate the already-installed `hub-daemon` binary recorded in the manifest.
/// Used when `--bin-src` isn't given this run (e.g. an app-bundle-only or
/// daemon-restart-only update) -- there is nothing new to copy, but the
/// daemon still needs a path to relaunch from.
fn recorded_daemon_path(m: &Manifest) -> Option<PathBuf> {
    m.binaries
        .iter()
        .map(PathBuf::from)
        .find(|p| p.file_name().and_then(|n| n.to_str()) == Some("hub-daemon"))
}

pub async fn run(
    home: &Path,
    yes: bool,
    bin_src: Option<&Path>,
    app_bundle: Option<&Path>,
) -> anyhow::Result<()> {
    let mut m = manifest::load(&paths::manifest_path(home))?;
    // `hub update` swaps binaries that must already be installed -- this
    // command only makes sense post-install. An empty (or missing/default)
    // manifest means there is nothing to update.
    if m.binaries.is_empty() {
        anyhow::bail!("hub is not installed; run `hub install` first");
    }

    let sock = paths::daemon_sock_path(home);
    // Best-effort; daemon may be down. Surfaced in the plan/summary only --
    // never a reason to abort (mirrors `uninstall::run`'s use of this call).
    let live = daemon_client::list_sessions(&sock).await.unwrap_or_default();

    if !yes {
        println!("hub update will:");
        match bin_src {
            Some(src) => println!(
                "  - replace hub, hub-daemon, hub-relay in {} from {}",
                paths::bin_dir(home).display(),
                src.display()
            ),
            None => println!("  - restart the daemon (no --bin-src given; binaries unchanged)"),
        }
        match app_bundle {
            Some(src) => {
                if let Some(name) = src.file_name() {
                    println!(
                        "  - replace /Applications/{} with {}",
                        Path::new(name).display(),
                        src.display()
                    );
                }
            }
            None => {
                if let Some(app) = &m.app_bundle {
                    println!("  - leave the existing app bundle unchanged: {app}");
                }
            }
        }
        println!(
            "  - restart the daemon; all {} live session(s) will keep running uninterrupted",
            live.len()
        );
        print!("Proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            anyhow::bail!("aborted by user");
        }
    }

    // 1. Binaries. Reuse `install::copy_binaries` verbatim: it already does
    //    the atomic per-file copy, the idempotent manifest recording, and
    //    persists after each recorded copy (see its own doc comment) -- no
    //    reason to duplicate any of that here.
    let daemon_path = match bin_src {
        Some(src) => install::copy_binaries(home, src, &mut m)?,
        None => recorded_daemon_path(&m).ok_or_else(|| {
            anyhow::anyhow!(
                "no hub-daemon binary recorded in the manifest and no --bin-src given; \
                 run `hub install --bin-src <dir>` first"
            )
        })?,
    };

    // 2. App bundle (optional; macOS only). `manifest.app_bundle` only
    //    records the /Applications DESTINATION `install_app_bundle` copied
    //    TO, never the build-output SOURCE `ditto` copied FROM -- so there is
    //    nothing to "re-ditto" from the manifest alone. Without an explicit
    //    `--app-bundle <path>` this run, any existing installed bundle is
    //    left untouched (matches `install::run`'s own non-fatal handling of
    //    this step: a failure here does not abort the rest of the update).
    if let Some(src) = app_bundle {
        match install::install_app_bundle(src) {
            Ok(dest) => {
                m.app_bundle = Some(dest.display().to_string());
                manifest::save(&paths::manifest_path(home), &m)?;
                println!("  - installed app: {}", dest.display());
            }
            Err(e) => eprintln!("hub: app bundle update skipped: {e:#}"),
        }
    }

    // 3. Restart the daemon PROCESS ONLY (see module doc for why this never
    //    disrupts a live session): stop the old one, wait for it to actually
    //    stop answering the socket, then launch the new binary detached. Its
    //    own startup re-adopts every still-running relay before it even
    //    binds the listener.
    let _ = daemon_client::shutdown_daemon(&sock).await;
    wait_daemon_down(&sock).await;
    spawn_daemon_detached(&daemon_path)?;
    let came_up = wait_daemon_up(&sock).await;
    if !came_up {
        eprintln!(
            "hub: warning: new daemon did not answer its socket within the poll budget; \
             it may still be starting -- check `hub status`. Live sessions are unaffected."
        );
    }

    // 4. Re-point autostart at the (possibly freshly-written, same-path)
    //    binary so future logins launch the updated daemon too.
    //    `autostart::install_autostart` already treats "already bootstrapped"
    //    as non-fatal (see its own doc comment), so re-running it here on an
    //    already-loaded label is safe.
    if m.autostart.is_some() {
        match autostart::install_autostart(&daemon_path, home) {
            Ok(entry) => {
                m.autostart = Some(entry);
                manifest::save(&paths::manifest_path(home), &m)?;
            }
            Err(e) => eprintln!("hub: autostart refresh skipped: {e:#}"),
        }
    }

    println!("hub updated.");
    if bin_src.is_some() {
        println!("  - binaries replaced in {}", paths::bin_dir(home).display());
    }
    println!("  - daemon restarted; {} live session(s) preserved", live.len());
    Ok(())
}
