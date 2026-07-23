use hub_proto::{Frame, ControlMsg, Origin, SessionId};
use hub_relay::paths::HubPaths;
use hub_tui::ViewerClient;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 { if p.exists() { return; } tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
}

#[tokio::test]
async fn viewer_client_attaches_and_echoes() {
    let dir = std::env::temp_dir().join(format!("vc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();
    std::env::set_var("HUB_DIR", dir.to_str().unwrap());

    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    wait_sock(&paths.daemon_sock()).await;

    let cfg = hub_relay::relay::RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm".into(), origin: Origin::External, title: "vc".into(),
    };
    let ds = paths.daemon_sock().to_string_lossy().to_string();
    // own_stdio=false: this test is embedded in the test-runner process, not a
    // standalone terminal, so the relay must not bridge the process's real stdin.
    tokio::spawn(async move { let _ = hub_relay::relay::run_relay(cfg, Some(ds), false).await; });
    wait_sock(&paths.sock(SessionId(1))).await;

    let mut vc = ViewerClient::connect(&paths.daemon_sock(), SessionId(1)).await.unwrap();
    // First frame is the Replay.
    match vc.recv().await.unwrap() {
        Some(Frame::Control(ControlMsg::Replay { id, .. })) => assert_eq!(id, SessionId(1)),
        other => panic!("expected Replay, got {other:?}"),
    }
    vc.send_input(b"yo\n").await;
    let mut echoed = false;
    for _ in 0..100 {
        if let Some(Frame::Data { bytes, .. }) = vc.recv().await.unwrap() {
            if String::from_utf8_lossy(&bytes).contains("yo") { echoed = true; break; }
        }
    }
    assert!(echoed);
}
