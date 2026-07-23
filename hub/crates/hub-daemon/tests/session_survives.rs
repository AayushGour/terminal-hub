//! Regression guard for the Task-6 interim-stub gap: after a relay registers,
//! the session must (a) still appear in a daemon `List`, and (b) its `<id>.sock`
//! must accept connections and answer a liveness `List` probe — both while the
//! relay lives. Task 6 left `serve_initial_channel`/`accept_session_socket` as
//! no-op stubs that dropped the daemon connection and the listener, making the
//! session vanish and the socket refuse connections. This test locks that shut.
//!
//! Kept in its own test file (own process) because it sets the process-global
//! `HUB_DIR` env var, which would race a sibling test in the same binary.

use hub_proto::{encode_control, ControlMsg, Frame, Origin, SessionId};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 { if p.exists() { return; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
}

#[tokio::test]
async fn session_survives_registration_and_id_sock_serves() {
    let dir = std::env::temp_dir().join(format!("survive-{}", std::process::id()));
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

    // Give the daemon a beat to finish registration, then List via the daemon.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // F1: `dial_hello` sends the mandatory `Hello { token }` first frame.
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.unwrap();
    let mut listed = false;
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Control(ControlMsg::Sessions { sessions })))) => {
                listed = sessions.iter().any(|s| s.id == SessionId(1));
                break;
            }
            Ok(Ok(Some(_))) => {}
            _ => {}
        }
    }
    assert!(listed, "session must remain in daemon List while relay is alive");

    // The relay's own <id>.sock must accept a connection and answer a List probe
    // (F1: authenticate with `Hello { token }` first, like a real probe).
    let (mut sfr, mut swr) = hub_relay::conn::dial_hello(&paths.sock(SessionId(1)), paths.base()).await
        .expect("<id>.sock must accept authenticated connections while relay is alive");
    write_frame(&mut swr, &encode_control(&ControlMsg::List)).await.unwrap();
    let mut probed = false;
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), sfr.next()).await {
            Ok(Ok(Some(Frame::Control(ControlMsg::Sessions { sessions })))) => {
                probed = sessions.iter().any(|s| s.id == SessionId(1));
                break;
            }
            Ok(Ok(Some(_))) => {}
            _ => {}
        }
    }
    assert!(probed, "<id>.sock must answer a List liveness probe with its own info");
}
