use hub_cli::reconcile::{reconcile, RecordFile};
use hub_proto::{Origin, SessionId, SessionInfo};
use std::path::Path;

fn si(id: u64) -> SessionInfo {
    SessionInfo {
        id: SessionId(id),
        origin: Origin::External,
        title: format!("s{id}"),
        pid: 100 + id as u32,
        started_unix: 0,
        cols: 80,
        rows: 24,
        cwd: String::new(),
        last_exit_code: None,
        activity_seq: 0,
    }
}

fn rec(id: u64, sock: &str) -> RecordFile {
    RecordFile { info: si(id), sock: sock.to_string() }
}

#[test]
fn buckets_split_healthy_ghost_orphan() {
    let live = vec![si(1), si(3)]; // daemon sees 1 and 3
    let records = vec![rec(1, "/s/1.sock"), rec(2, "/s/2.sock")]; // disk has 1 and 2
    // socket 2 is dead → ghost; 1 is healthy; 3 live but unrecorded → orphan.
    let alive = |p: &Path| p != Path::new("/s/2.sock");
    let b = reconcile(&live, &records, &alive);

    assert_eq!(b.healthy.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![1]);
    assert_eq!(b.ghost.iter().map(|r| r.info.id.0).collect::<Vec<_>>(), vec![2]);
    assert_eq!(b.orphan.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![3]);
}
