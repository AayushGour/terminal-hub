//! Relay-side backpressure / actor-loop tests:
//!  - I4: a blocked (Ctrl-S'd) primary stdout must NOT freeze the actor loop.
//!  - Detach-on-last-viewer: on Detach the relay stops streaming Output; a later
//!    Attach re-sends a Replay and resumes.
//!
//! These drive `run_actor` directly over its EventBus so we can simulate a stuck
//! primary consumer and observe viewer/daemon-channel delivery deterministically.

use hub_proto::{ControlMsg, Frame, FrameDecoder, SessionId};
use hub_relay::paths::HubPaths;
use hub_relay::relay::{
    build_event_bus, bridge_pty, run_actor, RelayActor, RelayConfig, RelayEvent, RelayState,
};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn cfg(origin: hub_proto::Origin) -> RelayConfig {
    RelayConfig {
        shell: "/bin/cat".into(),
        cwd: "/".into(),
        env: vec![],
        cols: 80,
        rows: 24,
        term: "xterm".into(),
        origin,
        title: "t".into(),
    }
}

fn tmp_paths(tag: &str) -> HubPaths {
    let dir = std::env::temp_dir().join(format!("bp-{}-{}-{}", tag, std::process::id(), nanos()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir);
    paths.ensure_dirs().unwrap();
    paths
}

fn nanos() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64
}

/// Pull the next decoded frame off a daemon-channel sink within `budget`.
async fn next_frame(
    rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    dec: &mut FrameDecoder,
    budget: Duration,
) -> Option<Frame> {
    if let Ok(Some(f)) = dec.next_frame() {
        return Some(f);
    }
    let start = Instant::now();
    while start.elapsed() < budget {
        match tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
            Ok(Some(bytes)) => {
                dec.push(&bytes);
                if let Ok(Some(f)) = dec.next_frame() {
                    return Some(f);
                }
            }
            Ok(None) => return None,
            Err(_) => {}
        }
    }
    None
}

/// I4: with the primary stdout channel FULL and its consumer never draining
/// (i.e. a Ctrl-S'd outer terminal), the actor must still serve viewers and
/// deliver shell echo — proving the primary write is off the actor loop.
#[tokio::test]
async fn primary_ctrl_s_does_not_freeze_viewers() {
    let paths = tmp_paths("prim");
    let (pty, out, screen) = RelayState::spawn_pty(&cfg(hub_proto::Origin::External)).unwrap();

    let ev = build_event_bus();
    let tx = ev.tx.clone();
    // Bridge cat's real pty output so echoed input flows back as Output events.
    bridge_pty(out, tx.clone());

    let mut actor = RelayActor::new(cfg(hub_proto::Origin::External), SessionId(1), paths, pty, screen);
    // Simulate a Ctrl-S'd outer terminal: a length-1 primary channel whose
    // consumer NEVER reads. With the OLD inline `stdout.write_all().await` a
    // stuck terminal would freeze the whole actor; with I4 the actor try_sends
    // (dropping on full) and keeps serving everyone else.
    let (ps_tx, ps_rx) = mpsc::channel::<Vec<u8>>(1);
    actor.primary_sink = Some(ps_tx);
    let _stuck_consumer = ps_rx; // hold it open but never drain -> stays full

    let actor_task = tokio::spawn(run_actor(actor, ev));

    // A viewer (daemon channel 7): we own the receiver.
    let (vtx, mut vrx) = mpsc::unbounded_channel::<Vec<u8>>();
    tx.send(RelayEvent::ChannelUp(7, vtx)).unwrap();
    tx.send(RelayEvent::Frame(7, Frame::Control(ControlMsg::Attach { id: SessionId(1) }))).unwrap();

    // Saturate the primary channel with many Output events (only 1 fits; the
    // rest get dropped by try_send). The actor MUST NOT block on any of these.
    for _ in 0..500 {
        tx.send(RelayEvent::Output(b"flood-flood-flood".to_vec())).unwrap();
    }

    // Push input via a viewer frame; cat echoes it back through the pty. The
    // echo must reach the viewer even though the primary is wedged.
    tx.send(RelayEvent::Frame(7, Frame::Data { id: SessionId(1), bytes: b"PINGMARK\n".to_vec() })).unwrap();

    let mut dec = FrameDecoder::default();
    let mut saw_data = false;
    let mut saw_ping = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        match next_frame(&mut vrx, &mut dec, Duration::from_millis(500)).await {
            Some(Frame::Data { bytes, .. }) => {
                saw_data = true;
                if String::from_utf8_lossy(&bytes).contains("PINGMARK") {
                    saw_ping = true;
                    break;
                }
            }
            Some(_) => {} // Replay etc.
            None => {}
        }
    }
    assert!(saw_data, "viewer must receive output frames while the primary is stuck");
    assert!(saw_ping, "input must reach the shell and echo to the viewer despite a stuck primary");

    tx.send(RelayEvent::Exit(None)).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), actor_task).await;
}

/// Detach-on-last-viewer: after a Detach the relay stops streaming Output; a
/// later Attach re-sends a Replay and Output resumes (lossless reconnect).
#[tokio::test]
async fn last_viewer_detach_stops_streaming() {
    let paths = tmp_paths("detach");
    let (pty, out, screen) = RelayState::spawn_pty(&cfg(hub_proto::Origin::Hub)).unwrap();
    // We feed Output manually (do NOT bridge the pty) to control exact timing.
    drop(out);

    let ev = build_event_bus();
    let tx = ev.tx.clone();
    let actor = RelayActor::new(cfg(hub_proto::Origin::Hub), SessionId(1), paths, pty, screen);
    let actor_task = tokio::spawn(run_actor(actor, ev));

    // Daemon channel 1 attaches.
    let (dtx, mut drx) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut dec = FrameDecoder::default();
    tx.send(RelayEvent::ChannelUp(1, dtx)).unwrap();
    tx.send(RelayEvent::Frame(1, Frame::Control(ControlMsg::Attach { id: SessionId(1) }))).unwrap();

    // Attach -> Replay.
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Control(ControlMsg::Replay { id, .. })) => assert_eq!(id, SessionId(1)),
        other => panic!("attach must yield a Replay, got {other:?}"),
    }

    // Streaming works while attached.
    tx.send(RelayEvent::Output(b"BEFORE".to_vec())).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Data { bytes, .. }) => assert!(String::from_utf8_lossy(&bytes).contains("BEFORE")),
        other => panic!("attached viewer must receive Output, got {other:?}"),
    }

    // Detach the (only) viewer, then produce more output: it must NOT be sent.
    tx.send(RelayEvent::Frame(1, Frame::Control(ControlMsg::Detach { id: SessionId(1) }))).unwrap();
    tx.send(RelayEvent::Output(b"AFTERDETACH".to_vec())).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_millis(600)).await {
        None => {} // expected: streaming stopped
        other => panic!("relay must stop streaming Output after last detach, got {other:?}"),
    }

    // Re-attach: a fresh Replay resyncs, and Output resumes.
    tx.send(RelayEvent::Frame(1, Frame::Control(ControlMsg::Attach { id: SessionId(1) }))).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Control(ControlMsg::Replay { .. })) => {}
        other => panic!("re-attach must yield a fresh Replay, got {other:?}"),
    }
    tx.send(RelayEvent::Output(b"RESUMED".to_vec())).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Data { bytes, .. }) => assert!(String::from_utf8_lossy(&bytes).contains("RESUMED")),
        other => panic!("Output must resume after re-attach, got {other:?}"),
    }

    tx.send(RelayEvent::Exit(None)).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), actor_task).await;
}
