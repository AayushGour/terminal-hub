mod common;
use common::{alive, load_record_retry, Harness};
use hub_proto::{Frame, SessionId};
use hub_tui::ViewerClient;
use std::time::Duration;

#[tokio::test]
async fn daemon_kill_does_not_kill_shell_and_reattach_works() {
    let mut h = Harness::start().await;

    // Detached Hub relay running a fake shell (`cat`) — survives everything but Kill.
    h.spawn_hub_relay("/bin/cat");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let rec = load_record_retry(&h.paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "relay should be running");

    // Attach a viewer via the daemon; prove the live path works first.
    {
        let mut vc = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
        // Consume the Replay.
        let _ = tokio::time::timeout(Duration::from_millis(500), vc.recv()).await;
        vc.send_input(b"pre\n").await;
        let mut ok = false;
        for _ in 0..80 {
            if let Ok(Ok(Some(Frame::Data { bytes, .. }))) =
                tokio::time::timeout(Duration::from_millis(200), vc.recv()).await {
                if String::from_utf8_lossy(&bytes).contains("pre") { ok = true; break; }
            }
        }
        assert!(ok, "live path works before daemon kill");
    }

    // *** SPOF: kill the daemon. ***
    h.kill_daemon();
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The relay AND its shell MUST still be alive — the daemon owns no pty.
    assert!(alive(relay_pid), "SPOF VIOLATION: relay died when daemon was killed");
    // Its per-session socket + record persist (relay is independent).
    assert!(h.paths.sock(SessionId(1)).exists(), "relay socket must persist");

    // Restart the daemon: reconciliation re-adopts the live relay (id preserved).
    h.restart_daemon().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Reattach through the restarted daemon and prove the SAME shell responds.
    let mut vc = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), vc.recv()).await; // Replay
    vc.send_input(b"post\n").await;
    let mut ok = false;
    for _ in 0..100 {
        if let Ok(Ok(Some(Frame::Data { bytes, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), vc.recv()).await {
            if String::from_utf8_lossy(&bytes).contains("post") { ok = true; break; }
        }
    }
    assert!(ok, "must reattach and reach the surviving shell after daemon restart");
    assert!(alive(relay_pid), "relay still alive after reattach");
}
