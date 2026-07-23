// Contract §J: `ControlMsg::Shutdown` must stop the daemon PROCESS while
// leaving relays (and their shells) completely untouched -- relays own the
// ptys and are the SPOF-surviving component by design. Mirrors the real-
// binary harness in `teardown_origin.rs`.
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
}
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// `run_relay` binds the per-session socket BEFORE it writes the record
/// file, so poll instead of a bare `.unwrap()` on the first attempt.
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
async fn shutdown_stops_daemon_process_but_relay_survives() {
    build();
    let dir = std::env::temp_dir().join(format!("tdshutdown-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    // Redirect the spawned daemon's stdio to null: a prior test hit an
    // orphan-hang from an inherited stdio pipe keeping the test process
    // waiting on output that never comes.
    let mut daemon = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_sock(&paths.daemon_sock()).await;

    // Detached hub-origin relay (double-fork): independent process, not a
    // child of the daemon, not a child of this test.
    std::process::Command::new(relay_bin())
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
    wait_sock(&paths.sock(SessionId(1))).await;

    let rec = load_record_retry(&paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "detached hub relay should be running before shutdown");

    // Send Shutdown to the daemon (F1: authenticate with Hello first).
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Shutdown)).await.unwrap();
    // The daemon closes the connection as its implicit ack; drain until EOF
    // (or a generous timeout) rather than assuming a reply frame.
    let _ = tokio::time::timeout(Duration::from_millis(1000), fr.next()).await;
    drop(wr);
    drop(fr);

    // (a) the daemon PROCESS must actually exit. Reap via try_wait (a
    // zombie's kill(pid,0) still succeeds, so we must reap to be sure).
    let mut gone = false;
    for _ in 0..400 {
        match daemon.try_wait() {
            Ok(Some(_)) => {
                gone = true;
                break;
            }
            Ok(None) => {}
            Err(_) => {
                gone = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "daemon process must exit on Shutdown");

    // hubd.sock must now refuse new connections (listener is gone).
    let mut refused = false;
    for _ in 0..40 {
        match tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::UnixStream::connect(paths.daemon_sock()),
        )
        .await
        {
            Ok(Err(_)) | Err(_) => {
                refused = true;
                break;
            }
            Ok(Ok(_)) => {}
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(refused, "hubd.sock must refuse connections after daemon shutdown");

    // (b) the relay must STILL BE ALIVE: relays survive daemon shutdown by
    // design (daemon is a router/registry with no pty; relays own the ptys).
    assert!(alive(relay_pid), "hub relay must survive daemon Shutdown");

    // Cleanup: the relay is detached (not our child) -- kill it directly so
    // we don't leak a live /bin/cat between test runs.
    unsafe {
        libc::kill(relay_pid as i32, libc::SIGKILL);
    }
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
