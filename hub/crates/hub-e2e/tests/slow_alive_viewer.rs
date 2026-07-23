//! Backpressure Fix 2 (cumulative-lag drop): a "slow-alive" viewer — one that
//! keeps reading, but only a frame every few hundred ms, never sustaining the
//! average output rate — must EVENTUALLY be dropped (bounding relay-side memory),
//! while a fast co-viewer keeps receiving output at full speed and is NOT
//! throttled down to the slow viewer's rate (no head-of-line blocking).
//!
//! Also (Fix 1): a viewer attaching DURING a flood must get a usable Replay or be
//! dropped + reconnectable — never kept-but-blank.

mod common;
use common::{alive, load_record_retry, Harness};
use hub_proto::{ControlMsg, Frame, SessionId};
use hub_tui::ViewerClient;
use std::time::{Duration, Instant};

/// Fast reader: drain as quickly as possible looking for `needle`, keeping only a
/// small rolling tail. Returns (saw_marker, total_bytes).
async fn drain_for_marker(vc: &mut ViewerClient, needle: &[u8], budget: Duration) -> (bool, usize) {
    let start = Instant::now();
    let mut tail: Vec<u8> = Vec::new();
    let mut total = 0usize;
    while start.elapsed() < budget {
        match tokio::time::timeout(Duration::from_millis(500), vc.recv()).await {
            Ok(Ok(Some(Frame::Data { bytes, .. }))) => {
                total += bytes.len();
                tail.extend_from_slice(&bytes);
                if tail.windows(needle.len()).any(|w| w == needle) { return (true, total); }
                if tail.len() > 4096 { let cut = tail.len() - 256; tail.drain(..cut); }
            }
            Ok(Ok(Some(_))) => {}                   // Replay/other control
            Ok(Ok(None)) => return (false, total),  // EOF
            Ok(Err(_)) => return (false, total),
            Err(_) => {}                            // no frame yet; keep waiting
        }
    }
    (false, total)
}

/// Slow-alive reader, then confirm-dropped.
///
/// Phase 1 (`slow_for`): read ONE frame, then dawdle ~300ms, in a loop. This
/// drains far slower than the flood fills, so the viewer stays perpetually behind
/// and the daemon's cumulative-lag rule must evict it (~LAG_GRACE).
///
/// Phase 2 (until `budget`): drain AS FAST AS POSSIBLE. A viewer that was dropped
/// hits its (bounded) socket backlog and then a clean EOF -> `true`. A viewer that
/// was NOT dropped keeps receiving the ongoing flood (or idles once it ends) and
/// never EOFs within budget -> `false` (a regression). The two phases are what
/// make the drop OBSERVABLE despite the OS socket send-buffer that a slow reader
/// would otherwise take a very long time to drain.
async fn slow_alive_then_confirm_dropped(vc: &mut ViewerClient, slow_for: Duration, budget: Duration) -> (bool, usize) {
    let start = Instant::now();
    let mut total = 0usize;
    // Phase 1: never keep up.
    while start.elapsed() < slow_for {
        match tokio::time::timeout(Duration::from_millis(500), vc.recv()).await {
            Ok(Ok(Some(Frame::Data { bytes, .. }))) => {
                total += bytes.len();
                tokio::time::sleep(Duration::from_millis(300)).await; // dawdle
            }
            Ok(Ok(Some(_))) => { tokio::time::sleep(Duration::from_millis(300)).await; }
            Ok(Ok(None)) | Ok(Err(_)) => return (true, total), // already dropped
            Err(_) => {}
        }
    }
    // Phase 2: drain flat-out; a dropped viewer reaches EOF quickly.
    while start.elapsed() < budget {
        match tokio::time::timeout(Duration::from_millis(500), vc.recv()).await {
            Ok(Ok(Some(Frame::Data { bytes, .. }))) => { total += bytes.len(); }
            Ok(Ok(Some(_))) => {}
            Ok(Ok(None)) | Ok(Err(_)) => return (true, total), // clean EOF -> dropped
            Err(_) => {}                                        // idle; keep waiting in budget
        }
    }
    (false, total)
}

/// Fix 2: slow-alive viewer eventually dropped; fast viewer NOT throttled to it.
#[tokio::test]
async fn slow_alive_viewer_eventually_dropped() {
    let h = Harness::start().await;
    h.spawn_hub_relay("/bin/sh");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let rec = load_record_retry(&h.paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "relay should be running");

    // A = fast reader, B = slow-alive reader, both on the SAME shell.
    let mut a = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let mut b = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), a.recv()).await; // Replay A
    let _ = tokio::time::timeout(Duration::from_millis(500), b.recv()).await; // Replay B

    // Sustained flood ending in a runtime-built marker (not echoed on the cmd line).
    a.send_input(b"seq 1 3000000; printf 'ZZ%sZZ\\n' DONE\n").await;

    // Drive A (fast) and B (slow-alive) CONCURRENTLY. If B head-of-line-blocked the
    // session, A could never reach the end marker within budget.
    let a_fut = drain_for_marker(&mut a, b"ZZDONEZZ", Duration::from_secs(60));
    // slow_for=8s (> LAG_GRACE=5s) guarantees B is evicted before phase 2.
    let b_fut = slow_alive_then_confirm_dropped(&mut b, Duration::from_secs(8), Duration::from_secs(40));
    let ((a_saw_end, a_bytes), (b_dropped, b_bytes)) = tokio::join!(a_fut, b_fut);

    // (i) The slow-alive viewer B was dropped (bounded relay memory), despite
    // actively reading the whole time.
    assert!(b_dropped, "slow-alive viewer B must be dropped within budget (saw {b_bytes} bytes)");

    // (ii) The fast viewer A kept getting output and reached the end of the
    // multi-MB flood — proving it was NOT throttled down to B's drip rate.
    assert!(a_saw_end, "fast viewer A must reach the end marker (got {a_bytes} bytes)");
    assert!(a_bytes > 8 * 1024 * 1024, "A should have drained the multi-MB flood, got {a_bytes} bytes");

    // (iii) The shell/relay survived the slow viewer being dropped.
    assert!(alive(relay_pid), "shell/relay must keep running after dropping a slow-alive viewer");
}

/// Fix 1 (e2e): a viewer attaching DURING a flood gets a usable Replay OR is
/// dropped (reconnectable) — never kept-but-blank (Output forever, no Replay, no
/// close). A deterministic version of the eviction path lives in the
/// `deliver_replay_evict` hub-daemon test.
#[tokio::test]
async fn attach_during_flood_gets_replay_or_dropped() {
    let h = Harness::start().await;
    h.spawn_hub_relay("/bin/sh");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let rec = load_record_retry(&h.paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;

    // A fast reader keeps the session live and the output flowing.
    let mut a = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), a.recv()).await; // Replay A
    a.send_input(b"seq 1 5000000; printf 'ZZ%sZZ\\n' FLOODEND\n").await;
    let a_task = tokio::spawn(async move {
        let _ = drain_for_marker(&mut a, b"ZZFLOODENDZZ", Duration::from_secs(60)).await;
    });

    // Attach a NEW viewer N mid-flood.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let mut n = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();

    // N must, within a bounded time, EITHER see a Replay control frame OR have its
    // connection closed. It must NOT be kept-but-blank (only Data, forever).
    let mut got_replay = false;
    let mut closed = false;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(30) {
        match tokio::time::timeout(Duration::from_millis(500), n.recv()).await {
            Ok(Ok(Some(Frame::Control(ControlMsg::Replay { .. })))) => { got_replay = true; break; }
            Ok(Ok(Some(_))) => {}                                   // Data delta; keep looking
            Ok(Ok(None)) | Ok(Err(_)) => { closed = true; break; } // dropped
            Err(_) => {}
        }
    }
    assert!(
        got_replay || closed,
        "viewer attaching during a flood must get a Replay or be dropped, never kept-but-blank"
    );

    let _ = a_task.await;
    assert!(alive(relay_pid), "shell/relay must survive attaches during a flood");
}
