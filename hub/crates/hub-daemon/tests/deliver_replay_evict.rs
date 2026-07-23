//! Fix 1 (deterministic): a viewer whose bounded channel is FULL when its Replay
//! arrives must be EVICTED, not silently kept with the Replay dropped (which would
//! leave it rendering Output deltas against a blank screen forever). Dropping it
//! closes its socket -> the client reconnects and gets a fresh Replay (lossless).
//! When it was the last viewer, the daemon must also forward Detach to the relay.

use hub_daemon::registry::{Registry, ViewerSink};
use hub_proto::{encode_control, ControlMsg, Origin, SessionId, SessionInfo};
use tokio::sync::{mpsc, oneshot};

fn info(id: SessionId) -> SessionInfo {
    SessionInfo { id, origin: Origin::Hub, title: "t".into(), pid: 0, started_unix: 0, cols: 80, rows: 24 }
}

#[tokio::test]
async fn deliver_replay_evicts_when_channel_full() {
    let reg = Registry::default();
    let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let id = SessionId(1);
    reg.add_session(info(id), relay_tx).await;

    // Viewer with a capacity-1 channel that we pre-fill so the Replay's try_send
    // is guaranteed to hit Full (the flood-fills-before-Replay race, deterministic
    // here). Keep the receiver alive so it's Full, not Closed.
    let (vtx, _vrx) = mpsc::channel::<Vec<u8>>(1);
    vtx.try_send(vec![0xFF]).unwrap(); // occupy the only slot
    let (kill, _killrx) = oneshot::channel::<()>();
    let sink = ViewerSink { tx: vtx, _kill: kill, behind_since: None };

    let vid = reg.attach_viewer(id, sink).await.ok().expect("attach viewer");
    // attach_viewer forwards an Attach to the relay; drain it.
    assert_eq!(relay_rx.recv().await.unwrap(), encode_control(&ControlMsg::Attach { id }));

    // Replay arrives while the channel is Full -> viewer must be evicted.
    reg.deliver_replay(id, vec![1, 2, 3]).await;

    // (1) The viewer is gone from the registry (evicted, not kept-but-blank).
    {
        let g = reg.inner.lock().await;
        let s = g.sessions.get(&id).expect("session still present");
        assert!(!s.viewers.contains_key(&vid), "viewer must be evicted when its Replay can't land");
        assert!(!s.pending_replay.contains(&vid), "evicted viewer must leave pending_replay");
    }

    // (2) It was the last viewer -> daemon forwards Detach to the relay.
    assert_eq!(
        relay_rx.recv().await.unwrap(),
        encode_control(&ControlMsg::Detach { id }),
        "last-viewer eviction must forward Detach so the relay stops streaming"
    );
}

#[tokio::test]
async fn deliver_replay_delivers_when_channel_has_room() {
    // Control case: a viewer that CAN accept its Replay is kept and receives it.
    let reg = Registry::default();
    let (relay_tx, mut relay_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let id = SessionId(1);
    reg.add_session(info(id), relay_tx).await;

    let (vtx, mut vrx) = mpsc::channel::<Vec<u8>>(4); // room to spare
    let (kill, _killrx) = oneshot::channel::<()>();
    let sink = ViewerSink { tx: vtx, _kill: kill, behind_since: None };
    let vid = reg.attach_viewer(id, sink).await.ok().expect("attach viewer");
    assert_eq!(relay_rx.recv().await.unwrap(), encode_control(&ControlMsg::Attach { id }));

    reg.deliver_replay(id, vec![9, 9, 9]).await;

    // Viewer kept, and it received exactly the Replay frame.
    {
        let g = reg.inner.lock().await;
        let s = g.sessions.get(&id).unwrap();
        assert!(s.viewers.contains_key(&vid), "a viewer with room must be kept");
    }
    assert_eq!(vrx.recv().await.unwrap(), encode_control(&ControlMsg::Replay { id, screen: vec![9, 9, 9] }));
    // No Detach forwarded (viewer still attached).
    assert!(relay_rx.try_recv().is_err(), "no Detach expected while the viewer is kept");
}
