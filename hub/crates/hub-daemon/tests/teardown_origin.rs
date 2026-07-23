use hub_proto::{encode_control, ControlMsg, SessionId};
use hub_relay::conn::write_frame;
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
    assert!(std::process::Command::new("cargo").args(["build","-p","hub-relay","-p","hub-daemon"]).status().unwrap().success());
}
async fn wait_sock(p: &std::path::Path) {
    for _ in 0..300 { if p.exists() { return; } tokio::time::sleep(Duration::from_millis(10)).await; }
}
fn alive(pid: u32) -> bool { unsafe { libc::kill(pid as i32, 0) == 0 } }

/// `run_relay` binds the per-session socket BEFORE it writes the record file,
/// so a test that waits on the socket and then immediately loads the record
/// can race the writer by a sub-millisecond window. Poll instead of a bare
/// `.unwrap()` on the first attempt.
async fn load_record_retry(p: &std::path::Path) -> hub_relay::record::SessionRecord {
    for _ in 0..50 {
        if let Ok(r) = hub_relay::record::SessionRecord::load(p) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    hub_relay::record::SessionRecord::load(p).expect("record never appeared")
}

#[tokio::test]
async fn external_dies_on_outer_pipe_close() {
    build();
    let dir = std::env::temp_dir().join(format!("tdext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    let mut daemon = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir).spawn().unwrap();
    wait_sock(&paths.daemon_sock()).await;

    // External relay: its stdin is the "outer terminal". Hold the pipe.
    let mut relay = std::process::Command::new(relay_bin())
        .args(["--origin","external","--shell","/bin/cat","--size","80x24",
               "--daemon-sock", paths.daemon_sock().to_str().unwrap()])
        .env("HUB_DIR", &dir)
        .stdin(Stdio::piped()).stdout(Stdio::piped())
        .spawn().unwrap();
    wait_sock(&paths.sock(SessionId(1))).await;
    let relay_pid = relay.id();
    assert!(alive(relay_pid));

    // Close the outer pipe (drop stdin) -> relay SIGHUPs cat and exits.
    drop(relay.stdin.take());
    // NOTE: the relay is a direct child of this test process and is not reaped
    // until we call wait/try_wait. A process that has exited but not been reaped
    // is a ZOMBIE, and `libc::kill(zombie_pid, 0)` still returns 0 ("alive").
    // So we must REAP to observe real termination: try_wait() returns Ok(Some(_))
    // only once the child has actually terminated. This proves the relay process
    // truly exited (not just that kill(pid,0) stopped succeeding).
    let mut gone = false;
    for _ in 0..200 {
        match relay.try_wait() {
            Ok(Some(_)) => { gone = true; break; } // relay process actually exited
            Ok(None) => {}                          // still running
            Err(_) => { gone = true; break; }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "External relay must die when the outer terminal closes");
    // Record cleaned up.
    assert!(!paths.record(SessionId(1)).exists());
    let _ = relay.wait();
    let _ = daemon.kill();
}

#[tokio::test]
async fn hub_survives_detach_dies_on_kill() {
    build();
    let dir = std::env::temp_dir().join(format!("tdhub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    let mut daemon = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir).spawn().unwrap();
    wait_sock(&paths.daemon_sock()).await;

    // Hub-origin, DETACHED relay (double-fork). stdin=/dev/null; no primary.
    std::process::Command::new(relay_bin())
        .args(["--detach","--origin","hub","--shell","/bin/cat","--size","80x24",
               "--daemon-sock", paths.daemon_sock().to_str().unwrap()])
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().unwrap();
    wait_sock(&paths.sock(SessionId(1))).await;

    let rec = load_record_retry(&paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "detached hub relay should be running");

    // Attach a viewer via the daemon (F1 Hello first), then DETACH -> must NOT kill.
    let (_fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id: SessionId(1) })).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    write_frame(&mut wr, &encode_control(&ControlMsg::Detach { id: SessionId(1) })).await.unwrap();
    drop(wr); drop(_fr); // viewer disconnects
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(alive(relay_pid), "Hub relay must survive viewer detach");

    // Explicit Kill -> relay SIGHUPs cat and exits. (F1 Hello first.)
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id: SessionId(1) })).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Kill { id: SessionId(1) })).await.unwrap();
    // Drain until Closed / EOF.
    let _ = tokio::time::timeout(Duration::from_millis(500), fr.next()).await;

    let mut gone = false;
    for _ in 0..200 { if !alive(relay_pid) { gone = true; break; } tokio::time::sleep(Duration::from_millis(25)).await; }
    assert!(gone, "Hub relay must die on explicit Kill");
    assert!(!paths.record(SessionId(1)).exists());
    let _ = daemon.kill();
}
