//! Detach-on-last-viewer (daemon side): when the LAST viewer for a session
//! detaches, the daemon must forward `ControlMsg::Detach{id}` to the relay so it
//! stops streaming Output into a void. A subsequent Attach must be forwarded
//! again (relay re-sends a Replay and resumes). We stand in as a fake relay and
//! observe exactly what the daemon forwards.

use hub_proto::{encode_control, ControlMsg, Frame, Origin};
use hub_relay::conn::{write_frame, FrameReader};
use hub_relay::paths::HubPaths;
use std::time::Duration;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 {
        if p.exists() { return; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {p:?}");
}

/// Read frames from the fake relay until a Control frame arrives (or timeout).
async fn next_control(
    fr: &mut FrameReader<tokio::net::unix::OwnedReadHalf>,
    budget: Duration,
) -> Option<ControlMsg> {
    let start = std::time::Instant::now();
    while start.elapsed() < budget {
        match tokio::time::timeout(Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Control(m)))) => return Some(m),
            Ok(Ok(Some(_))) => {}       // data frame; keep looking
            Ok(Ok(None)) => return None, // relay conn closed
            Ok(Err(_)) => return None,
            Err(_) => {}                 // idle; keep waiting
        }
    }
    None
}

#[tokio::test]
async fn daemon_forwards_detach_on_last_viewer() {
    let dir = std::env::temp_dir().join(format!("detachfwd-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    let p2 = paths.clone();
    tokio::spawn(async move { let _ = hub_daemon::server::run(p2).await; });
    wait_sock(&paths.daemon_sock()).await;

    // Fake relay: connect (F1 Hello first), Open a session, become the routing
    // target (relay_tx).
    let (mut rfr, mut rwr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    let open = ControlMsg::Open {
        shell: "/bin/cat".into(), cwd: "/".into(),
        cols: 80, rows: 24, term: "xterm".into(), origin: Origin::Hub, title: "t".into(),
    };
    write_frame(&mut rwr, &encode_control(&open)).await.unwrap();
    let id = loop {
        match next_control(&mut rfr, Duration::from_secs(2)).await {
            Some(ControlMsg::Opened { id }) => break id,
            Some(_) => continue,
            None => panic!("daemon never sent Opened"),
        }
    };

    // Viewer 1 attaches (F1 Hello first) -> daemon must forward Attach to the relay.
    let (_v1fr, mut v1wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut v1wr, &encode_control(&ControlMsg::Attach { id })).await.unwrap();
    match next_control(&mut rfr, Duration::from_secs(2)).await {
        Some(ControlMsg::Attach { id: aid }) => assert_eq!(aid, id),
        other => panic!("expected forwarded Attach, got {other:?}"),
    }

    // Viewer 1 (the only viewer) detaches -> daemon must forward Detach so the
    // relay stops streaming.
    write_frame(&mut v1wr, &encode_control(&ControlMsg::Detach { id })).await.unwrap();
    match next_control(&mut rfr, Duration::from_secs(2)).await {
        Some(ControlMsg::Detach { id: did }) => assert_eq!(did, id),
        other => panic!("expected forwarded Detach on last-viewer leave, got {other:?}"),
    }

    // A new viewer attaches (F1 Hello first) -> daemon forwards Attach again (streaming resumes).
    let (_v2fr, mut v2wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut v2wr, &encode_control(&ControlMsg::Attach { id })).await.unwrap();
    match next_control(&mut rfr, Duration::from_secs(2)).await {
        Some(ControlMsg::Attach { id: aid }) => assert_eq!(aid, id),
        other => panic!("expected forwarded Attach on re-attach, got {other:?}"),
    }

    let _ = std::fs::remove_dir_all(&dir);
}
