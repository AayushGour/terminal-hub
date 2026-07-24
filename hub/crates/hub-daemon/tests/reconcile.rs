use hub_daemon::reconcile::{probe_live, reconcile, Bucket};
use hub_proto::{encode_control, ControlMsg, Frame, Origin, SessionId, SessionInfo};
use hub_relay::conn::{write_frame, FrameReader};
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;

fn info(id: u64) -> SessionInfo {
    SessionInfo { id: SessionId(id), origin: Origin::External, title: "t".into(),
        pid: 1, started_unix: 0, cols: 80, rows: 24,
        cwd: String::new(), last_exit_code: None, activity_seq: 0 }
}

/// Bind a per-session socket that answers List with its own SessionInfo,
/// standing in for a live relay during a liveness probe.
async fn fake_live_relay(paths: &HubPaths, id: u64) {
    let sock = paths.sock(SessionId(id));
    let _ = std::fs::remove_file(&sock);
    let listener = tokio::net::UnixListener::bind(&sock).unwrap();
    let i = info(id);
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await { Ok(x) => x, Err(_) => break };
            let i = i.clone();
            tokio::spawn(async move {
                let (rd, mut wr) = stream.into_split();
                let mut fr = FrameReader::new(rd);
                // F1: the reconcile probe now sends `Hello { token }` as its
                // first frame; consume it, then answer the List liveness probe.
                let _ = fr.next().await; // Hello
                if let Ok(Some(Frame::Control(ControlMsg::List))) = fr.next().await {
                    let _ = write_frame(&mut wr, &encode_control(
                        &ControlMsg::Sessions { sessions: vec![i.clone()] })).await;
                }
            });
        }
    });
}

#[tokio::test]
async fn reconcile_buckets_healthy_ghost_orphan() {
    let dir = std::env::temp_dir().join(format!("recon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir);
    paths.ensure_dirs().unwrap();

    // 1 = healthy: record + live socket.
    SessionRecord { record_version: 1, id: SessionId(1), origin: Origin::External,
        title: "t".into(), pid: 1, started_unix: 0, cols: 80, rows: 24,
        sock: paths.sock(SessionId(1)).to_string_lossy().into(), cwd: String::new(), last_exit_code: None, activity_seq: 0 }.write_atomic(&paths).unwrap();
    fake_live_relay(&paths, 1).await;

    // 2 = ghost: record only, no socket.
    SessionRecord { record_version: 1, id: SessionId(2), origin: Origin::External,
        title: "t".into(), pid: 1, started_unix: 0, cols: 80, rows: 24,
        sock: paths.sock(SessionId(2)).to_string_lossy().into(), cwd: String::new(), last_exit_code: None, activity_seq: 0 }.write_atomic(&paths).unwrap();

    // 3 = orphan: live socket, no record.
    fake_live_relay(&paths, 3).await;

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let buckets = reconcile(&paths).await;

    let healthy: Vec<_> = buckets.iter().filter_map(|b| if let Bucket::Healthy(i) = b { Some(i.id.0) } else { None }).collect();
    let ghosts: Vec<_> = buckets.iter().filter_map(|b| if let Bucket::Ghost(id) = b { Some(id.0) } else { None }).collect();
    let orphans = buckets.iter().filter(|b| matches!(b, Bucket::Orphan(_))).count();

    assert_eq!(healthy, vec![1]);
    assert_eq!(ghosts, vec![2]);
    assert_eq!(orphans, 1);
}

/// Unique per-test scratch dir. Each test gets its own suffix (not just the
/// process id) so multiple `#[test]`s in this binary running in parallel
/// (default `cargo test` threading) never collide on the same directory —
/// unlike files that rely on a single process-wide `HUB_DIR` env var, this
/// file passes `HubPaths` directly into `reconcile`/`server::run`, so there
/// is no shared global state to race on as long as directories don't overlap.
fn unique_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("recon-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// TEST 1 — Ghost with a STALE DEAD socket (record present, `<id>.sock` file
/// exists on disk, but nothing is listening on it). This is the realistic
/// crashed-relay case: the socket file lingered after the relay process died
/// without cleaning up. Locks shut a regression where `sock.exists()` alone
/// (without a live probe) would be treated as healthy.
#[tokio::test]
async fn reconcile_buckets_ghost_with_stale_dead_socket() {
    let paths = HubPaths::new(unique_dir("stale-ghost"));
    paths.ensure_dirs().unwrap();

    // Record for id 9, socket path exists as a plain (non-listening) file.
    SessionRecord { record_version: 1, id: SessionId(9), origin: Origin::External,
        title: "t".into(), pid: 1, started_unix: 0, cols: 80, rows: 24,
        sock: paths.sock(SessionId(9)).to_string_lossy().into(), cwd: String::new(), last_exit_code: None, activity_seq: 0 }.write_atomic(&paths).unwrap();

    // Create a stale socket file: bind a real UnixListener, then drop it so
    // the inode/file lingers on disk with nothing accepting connections —
    // this mirrors what actually happens when a relay process is killed
    // (the OS doesn't always unlink the socket file), and (unlike a plain
    // `File::create`) still exercises the real "connect fails" path rather
    // than a filesystem-type mismatch.
    let sock_path = paths.sock(SessionId(9));
    {
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        drop(listener); // no one will ever accept() on this socket again
    }
    assert!(sock_path.exists(), "stale socket file should still exist on disk after listener drop");

    let buckets = reconcile(&paths).await;

    let healthy: Vec<_> = buckets.iter().filter_map(|b| if let Bucket::Healthy(i) = b { Some(i.id.0) } else { None }).collect();
    let ghosts: Vec<_> = buckets.iter().filter_map(|b| if let Bucket::Ghost(id) = b { Some(id.0) } else { None }).collect();

    assert!(healthy.is_empty(), "stale dead socket must never be reported healthy, got {healthy:?}");
    assert_eq!(ghosts, vec![9], "record with a dead/unresponsive socket must bucket as Ghost");
}

/// TEST 2 — `prune_ghost` actually removes the ghost's on-disk artifacts:
/// both the record `.json` and any lingering stale `.sock` file.
#[tokio::test]
async fn prune_ghost_removes_record_and_stale_socket_files() {
    let paths = HubPaths::new(unique_dir("prune-ghost"));
    paths.ensure_dirs().unwrap();

    SessionRecord { record_version: 1, id: SessionId(4), origin: Origin::External,
        title: "t".into(), pid: 1, started_unix: 0, cols: 80, rows: 24,
        sock: paths.sock(SessionId(4)).to_string_lossy().into(), cwd: String::new(), last_exit_code: None, activity_seq: 0 }.write_atomic(&paths).unwrap();

    let sock_path = paths.sock(SessionId(4));
    {
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        drop(listener);
    }
    let record_path = paths.record(SessionId(4));
    assert!(record_path.exists());
    assert!(sock_path.exists());

    // Drive the real reconcile flow first to confirm it lands in Ghost,
    // then invoke the cleanup path under test.
    let buckets = reconcile(&paths).await;
    let ghost_id = buckets.iter().find_map(|b| if let Bucket::Ghost(id) = b { Some(*id) } else { None });
    assert_eq!(ghost_id, Some(SessionId(4)), "expected id 4 bucketed as Ghost before pruning");

    hub_daemon::reconcile::prune_ghost(&paths, ghost_id.unwrap());

    assert!(!record_path.exists(), "prune_ghost should delete the ghost's record file");
    assert!(!sock_path.exists(), "prune_ghost should delete the ghost's stale socket file");
}

/// TEST 3 — id-floor regression (prevents id-hijack). A live ORPHAN holds
/// `sessions/5.sock` with no matching record. After the daemon reconciles at
/// startup, a fresh `Open` must be assigned an id strictly greater than the
/// orphan's, and must never delete/rebind the orphan's socket. This guards
/// the fix in `server::run` that seeds `next_id` past every bucket
/// (Healthy/Ghost/Orphan), not just the healthy ones.
#[tokio::test]
async fn reconcile_id_floor_prevents_orphan_id_hijack() {
    let paths = HubPaths::new(unique_dir("id-floor"));
    paths.ensure_dirs().unwrap();

    // Orphan: live relay-like listener on id 5's socket, no record file.
    fake_live_relay(&paths, 5).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Sanity: orphan socket is really there and really live before we start
    // the daemon, so any later disappearance is attributable to the daemon.
    let orphan_sock = paths.sock(SessionId(5));
    assert!(orphan_sock.exists());
    assert!(probe_live(&orphan_sock).await.is_some(), "orphan socket should answer List before daemon starts");

    // Start the real daemon against these paths (same pattern as
    // register_records.rs / open_list.rs): it runs its own reconcile() on
    // startup and must seed next_id past the orphan's id 5.
    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    for _ in 0..100 {
        if paths.daemon_sock().exists() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    // Give the daemon's startup reconcile a moment to finish (it awaits the
    // same probe_live used above) before we race it with Open.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Relay side: connect to the daemon (F1 Hello first) and Open a new session.
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    let open = ControlMsg::Open {
        shell: "/bin/cat".into(), cwd: "/".into(),
        cols: 80, rows: 24, term: "xterm-256color".into(),
        origin: Origin::External, title: "new".into(),
    };
    write_frame(&mut wr, &encode_control(&open)).await.unwrap();
    let id = match fr.next().await.unwrap() {
        Some(Frame::Control(ControlMsg::Opened { id })) => id,
        other => panic!("expected Opened, got {other:?}"),
    };

    assert!(id.0 > 5, "Open must not reassign/hijack the live orphan's id 5, got {}", id.0);

    // The orphan's socket must be untouched: still present, and still the
    // same live listener (not deleted+rebound by the daemon for the new id).
    assert!(orphan_sock.exists(), "orphan socket file must survive an unrelated Open");
    assert!(probe_live(&orphan_sock).await.is_some(), "orphan socket must remain live/answering after Open, not rebound to something else");
}
