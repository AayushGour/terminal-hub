//! `Registry::update_activity` (design spec 2026-07-23-shell-integration-design.md
//! §5): mutates the SAME in-memory `SessionInfo` map `Open`/`Opened` populate,
//! so it's immediately visible to the next `ControlMsg::List` -- no sockets
//! needed to exercise this directly. Same in-memory style as
//! `deliver_replay_evict.rs`.

use hub_daemon::registry::Registry;
use hub_proto::{Origin, SessionId, SessionInfo};
use tokio::sync::mpsc;

fn info(id: SessionId) -> SessionInfo {
    SessionInfo {
        id, origin: Origin::Hub, title: "t".into(), pid: 0, started_unix: 0, cols: 80, rows: 24,
        cwd: String::new(), last_exit_code: None, activity_seq: 0,
    }
}

#[tokio::test]
async fn update_activity_mutates_the_live_session_info() {
    let reg = Registry::default();
    let (relay_tx, _relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let id = SessionId(1);
    reg.add_session(info(id), relay_tx).await;

    reg.update_activity(id, "/home/u/proj".to_string(), Some(1), 4).await;

    let sessions = reg.list().await;
    let s = sessions.iter().find(|s| s.id == id).expect("session present");
    assert_eq!(s.cwd, "/home/u/proj");
    assert_eq!(s.last_exit_code, Some(1));
    assert_eq!(s.activity_seq, 4);

    // A later cwd-only update (no command finished) must not need to touch
    // activity_seq/last_exit_code in lockstep -- callers pass through
    // whatever the relay's actor-local state currently holds.
    reg.update_activity(id, "/home/u/proj/sub".to_string(), Some(1), 4).await;
    let sessions = reg.list().await;
    let s = sessions.iter().find(|s| s.id == id).unwrap();
    assert_eq!(s.cwd, "/home/u/proj/sub");
    assert_eq!(s.activity_seq, 4, "activity_seq unchanged by a bare cwd update");
}

#[tokio::test]
async fn update_activity_is_a_noop_for_an_unknown_session() {
    // A stale/racing SessionActivity for a session that already tore down
    // (or never existed) must not panic or create a phantom entry.
    let reg = Registry::default();
    reg.update_activity(SessionId(999), "/nowhere".to_string(), Some(0), 1).await;
    assert!(reg.list().await.is_empty());
}
