use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;
use hub_proto::SessionId;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn relay_registers_and_writes_record() {
    let dir = std::env::temp_dir().join(format!("reg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    for _ in 0..100 { if paths.daemon_sock().exists() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await; }

    // Run a relay in-process (External, fake shell cat), no detach.
    let cfg = hub_relay::relay::RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 100, rows: 30, term: "xterm".into(),
        origin: hub_proto::Origin::External, title: "reg".into(),
    };
    let sock = paths.daemon_sock().to_string_lossy().to_string();
    // HUB_DIR must point the relay at the same base dir.
    std::env::set_var("HUB_DIR", dir.to_str().unwrap());
    // In-process relay: it does NOT own the test process's stdin, so no primary bridge.
    tokio::spawn(async move { let _ = hub_relay::relay::run_relay(cfg, Some(sock), false).await; });

    // The record file should appear with a real pid and id 1.
    let mut ok = false;
    for _ in 0..200 {
        let rp = paths.record(SessionId(1));
        if rp.exists() {
            let rec = SessionRecord::load(&rp).unwrap();
            assert_eq!(rec.id, SessionId(1));
            assert!(rec.pid > 0);
            assert_eq!(rec.cols, 100);
            ok = true; break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ok, "record file should be written after registration");

    // F3: the per-session socket must be 0600 and sessions/ must be 0700 —
    // both dirs/sockets under ~/.hub must resist a shared-user peek even if
    // the base dir's mode were ever loosened.
    let sock_path = paths.sock(SessionId(1));
    let mut sock_ok = false;
    for _ in 0..50 {
        if sock_path.exists() { sock_ok = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(sock_ok, "session socket should exist after registration");
    let sock_mode = std::fs::metadata(&sock_path).unwrap().permissions().mode();
    assert_eq!(sock_mode & 0o777, 0o600, "session socket must be 0600, got {:o}", sock_mode & 0o777);

    let sessions_mode = std::fs::metadata(paths.sessions_dir()).unwrap().permissions().mode();
    assert_eq!(sessions_mode & 0o777, 0o700, "sessions/ dir must be 0700, got {:o}", sessions_mode & 0o777);
}
