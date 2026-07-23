use hub_proto::{encode_control, encode_data, ControlMsg, Frame, Origin, SessionId};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 { if p.exists() { return; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
}

#[tokio::test]
async fn viewer_attaches_replays_and_echoes() {
    let dir = std::env::temp_dir().join(format!("att-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();
    std::env::set_var("HUB_DIR", dir.to_str().unwrap());

    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    wait_sock(&paths.daemon_sock()).await;

    let cfg = hub_relay::relay::RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm".into(),
        origin: Origin::External, title: "s".into(),
    };
    let ds = paths.daemon_sock().to_string_lossy().to_string();
    // In-process relay: it does NOT own the test process's stdin, so no primary bridge.
    tokio::spawn(async move { let _ = hub_relay::relay::run_relay(cfg, Some(ds), false).await; });
    wait_sock(&paths.sock(SessionId(1))).await;

    // Viewer connects to the DAEMON (one app-facing socket) and attaches.
    // F1: `dial_hello` sends the mandatory `Hello { token }` first frame.
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id: SessionId(1) })).await.unwrap();

    // First frame back should be a Replay for session 1.
    let mut got_replay = false;
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Control(ControlMsg::Replay { id, .. })))) => { assert_eq!(id, SessionId(1)); got_replay = true; break; }
            Ok(Ok(Some(_))) => {}
            _ => {}
        }
    }
    assert!(got_replay, "attach must yield a Replay");

    // Send input; `cat` echoes; expect a Data frame carrying the echo.
    write_frame(&mut wr, &encode_data(SessionId(1), b"ping\n")).await.unwrap();
    let mut echoed = false;
    for _ in 0..100 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Data { id, bytes }))) => {
                assert_eq!(id, SessionId(1));
                if String::from_utf8_lossy(&bytes).contains("ping") { echoed = true; break; }
            }
            Ok(Ok(Some(_))) => {}
            _ => {}
        }
    }
    assert!(echoed, "input must echo back as Output over the daemon");
}
