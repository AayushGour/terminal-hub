//! Wiring test (design spec 2026-07-23-shell-integration-design.md §5/§7):
//! OSC 7/133 bytes arriving on `RelayEvent::Output` must (a) still reach an
//! attached viewer verbatim -- NOT stripped, unlike the stdin focus-report
//! fix (`strip_focus_reports`) which strips bytes on the INPUT side, (b)
//! update the actor's `SessionRecord` on disk, and (c) push a
//! `ControlMsg::SessionActivity` to the daemon over the relay's existing
//! channel(s) -- no new connection, no new polling loop.
//!
//! Same style as `sigwinch_wiring.rs`/`focus_report_wiring.rs`/
//! `backpressure.rs`: drives the real, non-fake pieces (`run_actor` over a
//! real `EventBus`, a real pty for `RelayActor::new` to own, the real
//! `hub_term::ShellIntegration` vte parse) rather than mocking.

use hub_proto::{ControlMsg, Frame, FrameDecoder, Origin, SessionId};
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;
use hub_relay::relay::{build_event_bus, run_actor, RelayActor, RelayConfig, RelayEvent, RelayState};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

fn cfg(origin: Origin) -> RelayConfig {
    RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm".into(), origin, title: "t".into(),
    }
}

fn tmp_paths(tag: &str) -> HubPaths {
    let dir = std::env::temp_dir().join(format!("si-wire-{}-{}-{}", tag, std::process::id(), nanos()));
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
async fn next_frame(rx: &mut mpsc::UnboundedReceiver<Vec<u8>>, dec: &mut FrameDecoder, budget: Duration) -> Option<Frame> {
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

#[tokio::test]
async fn osc_events_update_record_and_notify_daemon_without_stripping_output() {
    let paths = tmp_paths("basic");
    let (pty, out, screen) = RelayState::spawn_pty(&cfg(Origin::Hub)).unwrap();
    drop(out); // feed Output manually for exact control over the byte stream

    let ev = build_event_bus();
    let tx = ev.tx.clone();
    let id = SessionId(1);
    let actor = RelayActor::new(cfg(Origin::Hub), id, paths.clone(), pty, screen);
    let actor_task = tokio::spawn(run_actor(actor, ev));

    // One channel stands in for the daemon's own connection to this relay:
    // Attach so it also receives Output/Data (proves OSC bytes are forwarded,
    // not stripped), plus the unconditional SessionActivity broadcast.
    let (dtx, mut drx) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut dec = FrameDecoder::default();
    tx.send(RelayEvent::ChannelUp(1, dtx)).unwrap();
    tx.send(RelayEvent::Frame(1, Frame::Control(ControlMsg::Attach { id }))).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Control(ControlMsg::Replay { .. })) => {}
        other => panic!("attach must yield a Replay first, got {other:?}"),
    }

    // Synthetic pty output: prompt text, a cwd change (OSC 7, split across
    // the two Output events -- pty output arrives in arbitrary chunks), and
    // a finished command (OSC 133;D;0).
    let mut chunk1 = Vec::new();
    chunk1.extend_from_slice(b"$ cd /var/tmp\r\n");
    chunk1.extend_from_slice(b"\x1b]133;C\x07");
    chunk1.extend_from_slice(b"\x1b]7;file://host/var"); // split OSC7 mid-sequence
    let mut chunk2 = Vec::new();
    chunk2.extend_from_slice(b"/tmp\x07"); // completes OSC7
    chunk2.extend_from_slice(b"\x1b]133;D;0\x07");
    chunk2.extend_from_slice(b"$ ");

    tx.send(RelayEvent::Output(chunk1.clone())).unwrap();
    tx.send(RelayEvent::Output(chunk2.clone())).unwrap();

    // Drain every frame in ONE pass (Data and Control frames are interleaved
    // on this single channel -- a two-pass drain would silently discard
    // whichever frame kind the first pass wasn't looking for).
    let mut all_forwarded = Vec::new();
    let mut saw_cwd = false;
    let mut saw_finish = false;
    let want_bytes = chunk1.len() + chunk2.len();
    let deadline = Instant::now() + Duration::from_secs(3);
    while (all_forwarded.len() < want_bytes || !saw_cwd || !saw_finish) && Instant::now() < deadline {
        match next_frame(&mut drx, &mut dec, Duration::from_millis(500)).await {
            Some(Frame::Data { id: fid, bytes }) => { assert_eq!(fid, id); all_forwarded.extend(bytes); }
            Some(Frame::Control(ControlMsg::SessionActivity { id: aid, cwd, last_exit_code, activity_seq })) => {
                assert_eq!(aid, id);
                if cwd == "/var/tmp" && last_exit_code.is_none() && activity_seq == 0 { saw_cwd = true; }
                if last_exit_code == Some(0) && activity_seq == 1 { saw_finish = true; }
            }
            Some(_) => {}
            None => break,
        }
    }

    // (1) The raw OSC bytes must still reach the attached viewer verbatim --
    // NOT stripped, unlike stdin focus-report bytes.
    let forwarded_str = String::from_utf8_lossy(&all_forwarded);
    assert!(forwarded_str.contains("\x1b]7;file://host/var/tmp\x07"),
        "OSC 7 must not be stripped from viewer output: {forwarded_str:?}");
    assert!(forwarded_str.contains("\x1b]133;D;0\x07"),
        "OSC 133;D must not be stripped from viewer output: {forwarded_str:?}");

    // (2) SessionActivity must have been pushed to the daemon channel for
    // BOTH events: the cwd change (activity_seq NOT bumped) and the
    // command-finished (activity_seq bumped to 1).
    assert!(saw_cwd, "expected a SessionActivity reporting the new cwd");
    assert!(saw_finish, "expected a SessionActivity reporting the finished command");

    // (3) The on-disk SessionRecord must be rewritten to match (so a crashed
    // relay's ghost record still shows the last-known cwd/exit code).
    let rec = SessionRecord::load(&paths.record(id)).unwrap();
    assert_eq!(rec.cwd, "/var/tmp");
    assert_eq!(rec.last_exit_code, Some(0));
    assert_eq!(rec.activity_seq, 1);

    tx.send(RelayEvent::Exit(None)).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), actor_task).await;
}

/// OSC 133 A/C must be recognized+consumed with no `SessionActivity` at all
/// (spec §3: no handler logic beyond silent recognition) -- only D produces
/// one, and a bare cwd change produces one WITHOUT bumping `activity_seq`.
#[tokio::test]
async fn osc_133_a_and_c_produce_no_session_activity() {
    let paths = tmp_paths("ac-noop");
    let (pty, out, screen) = RelayState::spawn_pty(&cfg(Origin::Hub)).unwrap();
    drop(out);

    let ev = build_event_bus();
    let tx = ev.tx.clone();
    let id = SessionId(1);
    let actor = RelayActor::new(cfg(Origin::Hub), id, paths.clone(), pty, screen);
    let actor_task = tokio::spawn(run_actor(actor, ev));

    let (dtx, mut drx) = mpsc::unbounded_channel::<Vec<u8>>();
    let mut dec = FrameDecoder::default();
    tx.send(RelayEvent::ChannelUp(1, dtx)).unwrap();
    tx.send(RelayEvent::Frame(1, Frame::Control(ControlMsg::Attach { id }))).unwrap();
    match next_frame(&mut drx, &mut dec, Duration::from_secs(2)).await {
        Some(Frame::Control(ControlMsg::Replay { .. })) => {}
        other => panic!("attach must yield a Replay first, got {other:?}"),
    }

    tx.send(RelayEvent::Output(b"\x1b]133;A\x07\x1b]133;C\x07".to_vec())).unwrap();
    // Give the actor a moment to process, then confirm no SessionActivity
    // arrived (only the Data echo of the raw bytes, if anything).
    let deadline = Instant::now() + Duration::from_millis(400);
    let mut saw_activity = false;
    while Instant::now() < deadline {
        match next_frame(&mut drx, &mut dec, Duration::from_millis(100)).await {
            Some(Frame::Control(ControlMsg::SessionActivity { .. })) => saw_activity = true,
            _ => {}
        }
    }
    assert!(!saw_activity, "OSC 133 A/C alone must not produce a SessionActivity");

    tx.send(RelayEvent::Exit(None)).unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), actor_task).await;
}
