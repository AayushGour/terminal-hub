// Task 3 (reworked for Approach A): Backend viewer Tauri commands + shared
// `AppState`.
//
// Thin command layer: each `#[tauri::command]` fetches the `ConnManager` out of
// `AppState` and calls the matching per-session routing method (which opens /
// reuses that session's OWN connection — see `daemon.rs`). The wire encoding
// itself lives in a set of pure `build_*` helpers so it can be unit-tested
// without a Tauri runtime (no `State`, no webview) -- see `mod tests` below.

use std::sync::Arc;

use crate::daemon::ConnManager;
use hub_proto::{encode_control, encode_data, ControlMsg, SessionId, SessionInfo};
use tauri::State;

/// Shared app state: the per-tile `ConnManager`. Constructed once in
/// `lib.rs::run`'s `setup` (it needs the `AppHandle` for its event sink);
/// `None` only during the brief window before `setup` runs, so commands return
/// a clean "backend not initialized" error rather than panicking. There is no
/// app-wide daemon connection to be `Some`/`None` anymore — each open tile owns
/// its own connection inside the manager.
pub struct AppState {
    pub mgr: std::sync::Mutex<Option<Arc<ConnManager>>>,
}

// --- pure frame builders (unit-tested) ---

pub fn build_attach(id: u64) -> Vec<u8> {
    encode_control(&ControlMsg::Attach { id: SessionId(id) })
}
pub fn build_detach(id: u64) -> Vec<u8> {
    encode_control(&ControlMsg::Detach { id: SessionId(id) })
}
pub fn build_kill(id: u64) -> Vec<u8> {
    encode_control(&ControlMsg::Kill { id: SessionId(id) })
}
pub fn build_resize(id: u64, cols: u16, rows: u16) -> Vec<u8> {
    encode_control(&ControlMsg::Resize { id: SessionId(id), cols, rows })
}
pub fn build_claim_size(id: u64, cols: u16, rows: u16) -> Vec<u8> {
    encode_control(&ControlMsg::ClaimSize { id: SessionId(id), cols, rows })
}
pub fn build_input(id: u64, bytes: Vec<u8>) -> Vec<u8> {
    encode_data(SessionId(id), &bytes)
}

/// Fetch the `ConnManager`, or a clean error the UI can show (only possible in
/// the brief window before `setup` installs it). Never panics. A quick,
/// non-awaiting `std::sync::Mutex` lock: it just clones the `Arc` out.
pub(crate) fn manager(state: &State<'_, AppState>) -> Result<Arc<ConnManager>, String> {
    state
        .mgr
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "hub backend not initialized".to_string())
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, AppState>) -> Result<Vec<SessionInfo>, String> {
    manager(&state)?.list_sessions().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn attach(state: State<'_, AppState>, id: u64) -> Result<(), String> {
    manager(&state)?.attach(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn detach(state: State<'_, AppState>, id: u64) -> Result<(), String> {
    manager(&state)?.detach(id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn send_input(state: State<'_, AppState>, id: u64, bytes: Vec<u8>) -> Result<(), String> {
    manager(&state)?.send_input(id, bytes).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resize(state: State<'_, AppState>, id: u64, cols: u16, rows: u16) -> Result<(), String> {
    manager(&state)?.resize(id, cols, rows).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn claim_size(state: State<'_, AppState>, id: u64, cols: u16, rows: u16) -> Result<(), String> {
    manager(&state)?.claim_size(id, cols, rows).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn kill(state: State<'_, AppState>, id: u64) -> Result<(), String> {
    manager(&state)?.kill(id).await.map_err(|e| e.to_string())
}

// --- spawn_session (Hub origin) ---
//
// The daemon protocol has NO "spawn" wire message -- spec §E is explicit:
// "Hub-origin spawn is CLI/daemon-driven, NOT a wire message. There is no
// `ControlMsg::Spawn`." A Hub-origin session is created by launching a
// process that execs/spawns `hub-relay --origin hub --detach`, which then
// registers itself with the daemon (`Open` -> `Opened`) and writes its own
// `~/.hub/sessions/<id>.json` record; the new session shows up on the next
// `list_sessions` / `reconcile_sessions` poll. No auth work is needed here --
// the spawned relay authenticates itself against the daemon the same way
// `DaemonClient` does (reads `<HUB_DIR>/token`).
//
// DEVIATION FROM THE TASK-4 BRIEF'S LITERAL SNIPPET (`Command::new("hub")
// .args(["attach", "--new", "--origin", "hub"])`), documented per the
// brief's own "verify the exact flag name when Plan 3 lands" caveat:
// Plan 3's actual `hub attach` subcommand (`hub-cli/src/cli.rs::Command::
// Attach`) takes only a bare `--new` bool -- there is no `--origin` flag at
// all -- and `hub-cli/src/attach.rs::plan_attach` unconditionally execs the
// relay with `--origin external` hardcoded (it's the rc-injection path:
// `run_attach`/`plan_attach` size the pty from the CALLING process's own
// controlling tty via `TIOCGWINSZ` and are meant to be exec'd from an
// interactive shell, not spawned headless by a GUI). So `hub attach --new
// --origin hub` is not a real invocation of the shipped CLI; going through
// it here would either be a clap "unexpected argument" error or, if it
// somehow parsed, silently produce an External-origin session, not Hub.
//
// `INTERFACE-CONTRACT.md` §E gives the actual contract and explicitly
// names this as the GUI's job: "`hub attach --new --origin hub` (or GUI
// "new session") -> spawns `hub-relay --origin hub --detach`." -- i.e. the
// GUI is meant to invoke `hub-relay` directly with those flags, which is
// exactly what `spawn_session` below does (matching §D's canonical
// `hub-relay` CLI and the brief's own suggested fallback, "hub-relay
// --detach --origin hub ...").
//
// `hub-relay --detach` fully daemonizes itself in-process (setsid + double
// fork + reparent to init, see `hub_relay::detach::daemonize`) before
// starting its own tokio runtime; the process we `spawn()` here is only the
// *first* fork level, which exits quickly once its internal
// fork/waitpid/exit dance completes (see that module's own comment: "so the
// daemon's wait() reaps a clean child" -- here, we are that reaper). We
// don't block the Tauri command on it, but we do reap it off-thread so it
// never lingers as a zombie under this long-running app.

/// Pure: the argv `spawn_session` passes to the located `hub-relay` binary.
/// Kept separate from the actual `Command::spawn()` call so the "did we
/// build the right command" question is answerable without touching the
/// filesystem or process table.
pub fn build_spawn_relay_args(daemon_sock: &std::path::Path) -> Vec<String> {
    vec![
        "--origin".into(),
        "hub".into(),
        "--detach".into(),
        "--daemon-sock".into(),
        daemon_sock.display().to_string(),
    ]
}

/// Locate the `hub-relay` binary: first as a sibling of this app's own
/// executable (the installed-bundle / dev-tree convention documented in
/// `hub-cli/src/install.rs` as "assumption A5" -- daemon/relay binaries sit
/// beside `hub`), then by searching `$PATH`. Returns `None` (never panics)
/// if neither turns anything up, so the caller can surface a clean error
/// instead of a confusing OS-level "No such file or directory".
fn locate_relay_binary() -> Option<std::path::PathBuf> {
    let mut candidate_dirs = Vec::new();
    // (1) The installed location: `hub install --bin-src` drops the three
    // binaries in `<hub_home>/bin`. On a real Finder/launchd launch the app's
    // process `$PATH` does NOT include `~/.hub/bin` (that's only added to
    // interactive SHELL PATH by the rc snippet), and the binaries are NOT
    // beside the app exe (they're bundled under Contents/Resources) -- so
    // without this, "+ New session" (Hub spawn) can't find hub-relay after a
    // normal install. HUB_DIR-aware via hub_home(), so the sandbox demo also
    // finds its copied binaries here.
    candidate_dirs.push(crate::hub_home().join("bin"));
    // (2) Beside our own exe (dev tree / bundle sibling convention).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidate_dirs.push(dir.to_path_buf());
        }
    }
    // (3) Anything on $PATH (dev runs that export target/release, etc.).
    if let Some(path) = std::env::var_os("PATH") {
        candidate_dirs.extend(std::env::split_paths(&path));
    }
    find_binary_in(candidate_dirs.into_iter(), "hub-relay")
}

/// Pure(-ish) search: first `dir/name` that exists as a file, in order.
/// Factored out from `locate_relay_binary` so the "search candidate dirs"
/// logic is unit-testable against a controlled temp-dir list rather than
/// the real `current_exe()`/`$PATH`.
fn find_binary_in(dirs: impl Iterator<Item = std::path::PathBuf>, name: &str) -> Option<std::path::PathBuf> {
    dirs.map(|d| d.join(name)).find(|p| p.is_file())
}

/// Launch a Hub-origin session (Tauri "new session" action). See the block
/// comment above for why this spawns `hub-relay` directly rather than
/// shelling out to `hub attach --new`.
#[tauri::command]
pub fn spawn_session() -> Result<(), String> {
    let relay = locate_relay_binary()
        .ok_or_else(|| "hub-relay binary not found (not beside the app and not on PATH)".to_string())?;
    let daemon_sock = crate::hub_home().join("hubd.sock");

    let mut child = std::process::Command::new(&relay)
        .args(build_spawn_relay_args(&daemon_sock))
        .spawn()
        .map_err(|e| format!("failed to launch `{}`: {e}", relay.display()))?;

    // Reap off-thread (see block comment): the process we spawned exits on
    // its own in short order once `hub-relay --detach` finishes daemonizing;
    // we just need *something* to call `wait()` on it so it doesn't sit
    // around as a zombie for the lifetime of this long-running app.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{ControlMsg, Frame, FrameDecoder, SessionId};

    fn decode_one(bytes: &[u8]) -> Frame {
        let mut d = FrameDecoder::default();
        d.push(bytes);
        d.next_frame().unwrap().unwrap()
    }

    #[test]
    fn attach_frame_is_control_attach() {
        let f = decode_one(&build_attach(42));
        assert_eq!(f, Frame::Control(ControlMsg::Attach { id: SessionId(42) }));
    }

    #[test]
    fn input_frame_is_data() {
        let f = decode_one(&build_input(42, vec![b'l', b's', b'\n']));
        assert_eq!(f, Frame::Data { id: SessionId(42), bytes: b"ls\n".to_vec() });
    }

    #[test]
    fn claim_size_frame_carries_dims() {
        let f = decode_one(&build_claim_size(42, 120, 40));
        assert_eq!(f, Frame::Control(ControlMsg::ClaimSize { id: SessionId(42), cols: 120, rows: 40 }));
    }

    #[test]
    fn spawn_relay_args_are_hub_origin_detached_with_daemon_sock() {
        let args = build_spawn_relay_args(std::path::Path::new("/tmp/hub-test/hubd.sock"));
        assert_eq!(
            args,
            vec![
                "--origin".to_string(),
                "hub".to_string(),
                "--detach".to_string(),
                "--daemon-sock".to_string(),
                "/tmp/hub-test/hubd.sock".to_string(),
            ]
        );
    }

    #[test]
    fn find_binary_in_returns_first_existing_candidate() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Only `b` actually has the binary; `a` must be skipped over, not
        // returned as a false match, and not cause a panic.
        std::fs::write(b.path().join("hub-relay"), b"#!/bin/sh\n").unwrap();

        let found = find_binary_in(vec![a.path().to_path_buf(), b.path().to_path_buf()].into_iter(), "hub-relay");
        assert_eq!(found, Some(b.path().join("hub-relay")));
    }

    #[test]
    fn find_binary_in_returns_none_when_absent_everywhere() {
        let a = tempfile::tempdir().unwrap();
        let found = find_binary_in(std::iter::once(a.path().to_path_buf()), "hub-relay");
        assert_eq!(found, None);
    }
}
