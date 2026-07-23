// Contract §J: daemon singleton guard. `hub_transport::bind_listener`
// unconditionally unlinks a stale `hubd.sock`, so without a guard a 2nd
// daemon started under the same HUB_DIR would silently steal the socket
// from a live one. This test proves: (1) a 2nd daemon under the same
// HUB_DIR fails to start, and (2) the 1st daemon's socket is never stolen --
// it keeps answering List throughout.
use hub_proto::{encode_control, ControlMsg, Frame};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;
use std::process::Stdio;
use std::time::Duration;

fn daemon_bin() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-daemon");
    p
}
fn build() {
    assert!(std::process::Command::new("cargo")
        .args(["build", "-p", "hub-daemon"])
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

/// Connect to `hubd.sock`, send List, and confirm we get a Sessions reply
/// back -- proof the socket is bound to a live, responsive daemon.
async fn list_ok(paths: &HubPaths) -> bool {
    // F1: authenticate with `Hello { token }` first (via `dial_hello`), then List.
    let (mut fr, mut wr) = match tokio::time::timeout(
        Duration::from_secs(2),
        hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()),
    )
    .await
    {
        Ok(Ok(x)) => x,
        _ => return false,
    };
    if write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.is_err() {
        return false;
    }
    matches!(
        tokio::time::timeout(Duration::from_secs(2), fr.next()).await,
        Ok(Ok(Some(Frame::Control(ControlMsg::Sessions { .. }))))
    )
}

#[tokio::test]
async fn second_daemon_under_same_hub_dir_is_rejected() {
    build();
    let dir = std::env::temp_dir().join(format!("tdsingleton-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    // Daemon 1: starts normally, binds hubd.sock, holds the lock.
    let mut daemon1 = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_sock(&paths.daemon_sock()).await;
    assert!(list_ok(&paths).await, "daemon 1 must answer List before daemon 2's attempt");

    // Daemon 2: same HUB_DIR, started while daemon 1 is still alive. It must
    // fail to acquire the singleton lock and exit non-zero WITHOUT unlinking
    // or rebinding daemon 1's hubd.sock.
    let status2 = std::process::Command::new(daemon_bin())
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        !status2.success(),
        "a 2nd daemon under the same HUB_DIR must fail to start (exit non-zero)"
    );

    // Daemon 1 must still be alive and serving -- its socket was never
    // stolen by daemon 2's failed startup attempt.
    assert!(
        daemon1.try_wait().unwrap().is_none(),
        "daemon 1 must still be running after daemon 2's failed startup attempt"
    );
    assert!(
        list_ok(&paths).await,
        "daemon 1 must still answer List after daemon 2's failed startup attempt"
    );

    let _ = daemon1.kill();
    let _ = daemon1.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
