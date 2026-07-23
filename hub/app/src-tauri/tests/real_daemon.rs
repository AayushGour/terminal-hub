// Real-daemon integration proof for the Approach-A rework (per-tile
// connections). This drives the ACTUAL `hub-daemon` + `hub-relay` binaries
// (not the mock) through the app's backend command layer (`ConnManager`) and
// proves the four things the mock could NEVER prove — because the mock let one
// shared connection multiplex every session, exactly the design that is wrong
// against the real daemon:
//
//   (a) attach streams THAT session's output;
//   (b) TWO sessions attached simultaneously each get their OWN output
//       independently (per-tile isolation — impossible on one shared conn,
//       where only the first-attached tile would ever stream);
//   (c) kill(id) actually kills that session's relay WITHOUT touching the
//       other (isolation on the control path too);
//   (d) detach(id) does NOT kill — the shell keeps running.
//
// Harness mirrors `hub-daemon/tests/teardown_origin.rs` and
// `hub-cli/tests/kill_session_real.rs`: temp HUB_DIR, real daemon + two
// hub-origin `/bin/cat` relays (a pty running `cat` echoes input straight back
// as output, which is what lets us assert on per-session streaming).

use hub_app_lib::daemon::{ConnManager, EventSink};
use hub_proto::SessionId;
use hub_relay::paths::HubPaths;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
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

/// `run_relay` binds the per-session socket BEFORE it writes the record file,
/// so poll instead of a bare `.unwrap()` (mirrors the other real harnesses).
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

/// Collecting event sink standing in for the Tauri webview: records every
/// (event, payload) the manager's per-session readers emit.
#[derive(Clone)]
struct CollectSink(Arc<Mutex<Vec<(String, serde_json::Value)>>>);
impl EventSink for CollectSink {
    fn emit_json(&self, event: &'static str, payload: serde_json::Value) {
        self.0.lock().unwrap().push((event.to_string(), payload));
    }
}

/// True if some `hub://output` event for `id` carries `needle` in its bytes.
fn output_has(events: &Arc<Mutex<Vec<(String, serde_json::Value)>>>, id: u64, needle: &[u8]) -> bool {
    events.lock().unwrap().iter().any(|(ev, p)| {
        ev == "hub://output"
            && p["id"] == id
            && p["bytes"]
                .as_array()
                .map(|a| a.iter().map(|n| n.as_u64().unwrap() as u8).collect::<Vec<u8>>())
                .map(|b| b.windows(needle.len()).any(|w| w == needle))
                .unwrap_or(false)
    })
}

async fn wait_output(
    events: &Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    id: u64,
    needle: &[u8],
) -> bool {
    for _ in 0..300 {
        if output_has(events, id, needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Spawn a detached hub-origin relay running `/bin/cat`. Returns once its
/// per-session socket + record exist; yields the relay pid.
async fn spawn_cat_relay(paths: &HubPaths, dir: &std::path::Path, id: SessionId) -> u32 {
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
        .env("HUB_DIR", dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "relay launcher failed to spawn/detach");
    wait_sock(&paths.sock(id)).await;
    load_record_retry(&paths.record(id)).await.pid
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_daemon_two_session_isolation_kill_and_detach() {
    build();

    let dir = std::env::temp_dir().join(format!("hub-app-real-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    // Real daemon. Stdio -> null so a mid-test panic can't wedge the harness's
    // stdout pipe (per the note in kill_session_real.rs).
    let mut daemon = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_sock(&paths.daemon_sock()).await;

    // Two hub-origin cat relays. Spawn sequentially so ids are deterministic
    // (first Open -> session 1, second -> session 2).
    let pid1 = spawn_cat_relay(&paths, &dir, SessionId(1)).await;
    let pid2 = spawn_cat_relay(&paths, &dir, SessionId(2)).await;
    assert!(alive(pid1) && alive(pid2), "both relays should be running before attach");

    // The app's backend command layer, pointed at the REAL daemon socket.
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink: Arc<dyn EventSink> = Arc::new(CollectSink(events.clone()));
    let mgr = ConnManager::new(paths.daemon_sock(), sink);

    // Confirm both sessions are registered before attaching (avoids racing the
    // relay's Open registration with our Attach).
    let listed = mgr.list_sessions().await.unwrap();
    assert!(
        listed.iter().any(|s| s.id == SessionId(1)) && listed.iter().any(|s| s.id == SessionId(2)),
        "daemon must list both sessions before attach: {listed:?}"
    );

    // (a)+(b): attach BOTH tiles simultaneously — each opens its OWN connection.
    mgr.attach(1).await.unwrap();
    mgr.attach(2).await.unwrap();

    // Drive distinct input into each; a cat-in-a-pty echoes it back as output.
    mgr.send_input(1, b"AAA\n".to_vec()).await.unwrap();
    mgr.send_input(2, b"BBB\n".to_vec()).await.unwrap();

    // Each session streams its OWN output independently...
    assert!(wait_output(&events, 1, b"AAA").await, "session 1 must stream its own output");
    assert!(wait_output(&events, 2, b"BBB").await, "session 2 must stream its own output");
    // ...and never the other's (proves per-tile isolation, not one shared conn).
    assert!(!output_has(&events, 1, b"BBB"), "session 1 must not receive session 2's output");
    assert!(!output_has(&events, 2, b"AAA"), "session 2 must not receive session 1's output");

    // (c) kill(1): session 1 is attached, so Kill goes on its OWN connection.
    // The relay must die and — critically — session 2 must be untouched.
    mgr.kill(1).await.unwrap();
    let mut s1_gone = false;
    for _ in 0..200 {
        if !alive(pid1) {
            s1_gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(s1_gone, "kill(1) must terminate session 1's relay (pid {pid1})");
    assert!(alive(pid2), "kill(1) must NOT affect session 2's relay (pid {pid2})");

    // (d) detach(2): stop viewing, but the shell keeps running (detach != kill).
    mgr.detach(2).await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(alive(pid2), "detach(2) must NOT kill session 2's relay (pid {pid2})");

    // Cleanup: session 2 is no longer attached, so kill(2) takes the
    // short-lived Attach->Kill path (mirrors killing a ghost/orphan).
    let _ = mgr.kill(2).await;
    for _ in 0..200 {
        if !alive(pid2) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let _ = daemon.kill();
    let _ = std::fs::remove_dir_all(&dir);
}
