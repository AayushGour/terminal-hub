use hub_proto::{encode_control, encode_data, ControlMsg, Frame, Origin, SessionId};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 { if p.exists() { return; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await; }
}

#[tokio::test]
async fn claim_size_snaps_pty_dimensions() {
    let dir = std::env::temp_dir().join(format!("fsz-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();
    std::env::set_var("HUB_DIR", dir.to_str().unwrap());

    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    wait_sock(&paths.daemon_sock()).await;

    // Fake shell = an interactive `sh` (a real tty), so `stty size` works.
    let cfg = hub_relay::relay::RelayConfig {
        shell: "/bin/sh".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm".into(), origin: Origin::External, title: "sz".into(),
    };
    let ds = paths.daemon_sock().to_string_lossy().to_string();
    // In-process relay: it does NOT own the test process's stdin, so no primary bridge.
    tokio::spawn(async move { let _ = hub_relay::relay::run_relay(cfg, Some(ds), false).await; });
    wait_sock(&paths.sock(SessionId(1))).await;

    // F1: `dial_hello` sends the mandatory `Hello { token }` first frame.
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id: SessionId(1) })).await.unwrap();

    // Focus-gain claim: 120x40.
    write_frame(&mut wr, &encode_control(&ControlMsg::ClaimSize { id: SessionId(1), cols: 120, rows: 40 })).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(120)).await; // > 50ms debounce

    // Ask the shell to report its window size.
    write_frame(&mut wr, &encode_data(SessionId(1), b"stty size\n")).await.unwrap();

    let mut saw = false;
    for _ in 0..150 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Data { bytes, .. }))) => {
                if String::from_utf8_lossy(&bytes).contains("40 120") { saw = true; break; }
            }
            _ => {}
        }
    }
    assert!(saw, "pty must report the claimed 40 rows x 120 cols");
}
