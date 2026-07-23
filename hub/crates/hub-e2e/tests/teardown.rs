mod common;
use common::{alive, load_record_retry, Harness};
use hub_proto::SessionId;
use hub_tui::ViewerClient;
use std::time::Duration;

#[tokio::test]
async fn external_outer_pipe_close_kills_shell() {
    let h = Harness::start().await;
    let mut relay = h.spawn_external_relay("/bin/cat");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let pid = relay.id();
    assert!(alive(pid));

    // A viewer attaches then detaches: must NOT affect an External session.
    {
        let mut vc = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
        let _ = tokio::time::timeout(Duration::from_millis(300), vc.recv()).await;
        vc.detach().await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(alive(pid), "External session survives viewer detach");

    // Close the OUTER terminal pipe -> External session dies.
    drop(relay.stdin.take());
    // `relay` is a DIRECT child of this test process (External relays are not
    // detached), so on exit it becomes a zombie until reaped — and
    // `libc::kill(zombie, 0)` still returns 0 ("alive"). Poll `try_wait`, which
    // reaps it, so we observe the real termination rather than the zombie.
    let mut gone = false;
    for _ in 0..200 {
        if let Ok(Some(_)) = relay.try_wait() { gone = true; break; }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "External session dies when outer terminal closes");
    let _ = pid; // pid used only for the pre-teardown liveness assertions above
}

#[tokio::test]
async fn hub_detach_does_not_kill() {
    let h = Harness::start().await;
    h.spawn_hub_relay("/bin/cat");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let pid = load_record_retry(&h.paths.record(SessionId(1))).await.pid;

    let mut vc = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(300), vc.recv()).await;
    vc.detach().await;
    drop(vc);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(alive(pid), "Hub session must survive viewer detach (closing hub-tui != kill)");
}
