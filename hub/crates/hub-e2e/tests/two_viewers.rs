mod common;
use common::Harness;
use hub_proto::{Frame, SessionId};
use hub_tui::ViewerClient;
use std::time::Duration;

async fn drain_for<'a>(vc: &mut ViewerClient, needle: &str) -> bool {
    for _ in 0..120 {
        if let Ok(Ok(Some(Frame::Data { bytes, .. }))) =
            tokio::time::timeout(Duration::from_millis(200), vc.recv()).await {
            if String::from_utf8_lossy(&bytes).contains(needle) { return true; }
        }
    }
    false
}

#[tokio::test]
async fn two_viewers_mirror_one_shell() {
    let h = Harness::start().await;
    h.spawn_hub_relay("/bin/cat");
    h.wait_path(&h.paths.sock(SessionId(1))).await;

    let mut a = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let mut b = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(400), a.recv()).await; // Replay A
    let _ = tokio::time::timeout(Duration::from_millis(400), b.recv()).await; // Replay B

    // Input from A reaches the shell; output mirrors to BOTH.
    a.send_input(b"fromA\n").await;
    assert!(drain_for(&mut a, "fromA").await, "A sees its own echo");
    assert!(drain_for(&mut b, "fromA").await, "B mirrors A's output");

    // Input from B reaches the shell; output mirrors to BOTH.
    b.send_input(b"fromB\n").await;
    assert!(drain_for(&mut b, "fromB").await, "B sees its own echo");
    assert!(drain_for(&mut a, "fromB").await, "A mirrors B's output");
}
