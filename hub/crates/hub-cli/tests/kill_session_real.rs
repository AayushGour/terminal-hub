// Real end-to-end proof for Task 8's `kill_session` fix.
//
// This test drives the ACTUAL `hub-daemon` + `hub-relay` binaries (not a
// fake/mock daemon) exactly the way `hub-daemon/tests/teardown_origin.rs`
// does, then calls `hub_cli::daemon_client::kill_session` against them and
// asserts the relay process is really dead afterward.
//
// This MUST fail against the old `kill_session` (which sent a bare
// `ControlMsg::Kill{id}` as the first frame): the real daemon's
// `handle_conn` only accepts `Open`/`List`/`Attach` as a first frame, so the
// old code's connection got silently dropped by the catch-all arm, `Err`
// came back as "connection closed by peer", and the relay process was never
// touched. Against the fixed `kill_session` (Attach-then-Kill on the same
// connection, mirroring `hub_survives_detach_dies_on_kill`), the relay
// actually receives and processes the Kill and exits.

use hub_cli::daemon_client::{kill_session, list_sessions};
use hub_proto::SessionId;
use hub_relay::paths::HubPaths;
use std::process::Stdio;
use std::time::Duration;

fn relay_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-relay");
    p
}
fn daemon_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-daemon");
    p
}
fn build() {
    assert!(std::process::Command::new("cargo")
        .args(["build", "-p", "hub-relay", "-p", "hub-daemon"])
        .status()
        .unwrap()
        .success());
}

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..300 {
        if p.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("socket never appeared: {p:?}");
}

/// `run_relay` binds the per-session socket BEFORE it writes the record
/// file, so a caller that waits on the socket and then immediately loads
/// the record can race the writer by a sub-millisecond window. Poll instead
/// of a bare `.unwrap()` on the first attempt (mirrors
/// `teardown_origin.rs::load_record_retry`).
async fn load_record_retry(p: &std::path::Path) -> hub_relay::record::SessionRecord {
    for _ in 0..50 {
        if let Ok(r) = hub_relay::record::SessionRecord::load(p) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    hub_relay::record::SessionRecord::load(p).expect("record never appeared")
}

fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[tokio::test]
async fn kill_session_actually_kills_the_relay() {
    build();

    let dir = std::env::temp_dir().join(format!("hub-kill-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    // Spawn the real daemon. Stdio is redirected to null (rather than
    // inherited) so that if a later assertion in this test panics before
    // `daemon.kill()` runs, the orphaned daemon process doesn't keep the
    // test harness's stdout pipe open and hang whatever invoked `cargo
    // test` (observed empirically while validating this test against the
    // pre-fix `kill_session`).
    let mut daemon = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_sock(&paths.daemon_sock()).await;

    // Spawn a hub-origin, DETACHED relay (double-fork, reparented to init —
    // no zombie/reap concerns for this test). stdin/stdout/stderr -> null.
    let status = std::process::Command::new(relay_bin())
        .args([
            "--detach",
            "--origin",
            "hub",
            "--shell",
            "/bin/cat",
            "--size",
            "80x24",
            "--daemon-sock",
            paths.daemon_sock().to_str().unwrap(),
        ])
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "relay launcher process failed to spawn/detach");

    wait_sock(&paths.sock(SessionId(1))).await;
    let rec = load_record_retry(&paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "detached hub relay should be running before kill");

    // Sanity: the daemon actually knows about the session before we kill it.
    let before = list_sessions(&paths.daemon_sock()).await.unwrap();
    assert!(
        before.iter().any(|s| s.id == SessionId(1)),
        "daemon should list session 1 before kill: {before:?}"
    );

    // The fix under test: this used to send a bare Kill as the first frame
    // and get silently dropped by the daemon, returning
    // Err("connection closed by peer") and killing nothing.
    let result = kill_session(&paths.daemon_sock(), SessionId(1)).await;
    assert!(
        result.is_ok(),
        "kill_session must return Ok (or best-effort success), got: {result:?}"
    );

    // Prove the relay process is REALLY dead (not just that the daemon
    // dropped its record) by polling libc::kill(pid, 0) -> ESRCH.
    let mut gone = false;
    for _ in 0..200 {
        if !alive(relay_pid) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "kill_session must actually terminate the relay process (pid {relay_pid})");

    // The session record must be cleaned up by the relay's own teardown.
    assert!(
        !paths.record(SessionId(1)).exists(),
        "session record must be removed once the relay exits"
    );

    // And the daemon's live view must no longer list the killed session.
    let mut listed_gone = false;
    for _ in 0..100 {
        let after = list_sessions(&paths.daemon_sock()).await.unwrap();
        if !after.iter().any(|s| s.id == SessionId(1)) {
            listed_gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(listed_gone, "list_sessions must no longer report the killed session");

    let _ = daemon.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
