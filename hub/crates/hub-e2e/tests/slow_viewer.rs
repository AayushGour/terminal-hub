//! I3/F4: a viewer that stops reading must be DROPPED (bounded memory), while
//! other viewers and the shell keep running. A terminal byte-stream can't drop
//! arbitrary bytes, so backpressure = bound the per-viewer buffer and disconnect
//! the slow viewer; it reconnects and gets a fresh Replay to resync losslessly.

mod common;
use common::{alive, load_record_retry, Harness};
use hub_proto::{Frame, SessionId};
use hub_tui::ViewerClient;
use std::time::{Duration, Instant};

/// Drain a viewer looking for the raw byte marker `needle`, keeping only a small
/// rolling tail (so a multi-MB flood neither blows up memory nor misses a marker
/// spanning a chunk boundary). Also returns the total bytes seen. This forces
/// the caller to consume the WHOLE flood (the marker is emitted last), which is
/// what gives the stalled viewer time to overflow its bound.
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
            Ok(Ok(Some(_))) => {}          // Replay/other control
            Ok(Ok(None)) => return (false, total), // unexpected EOF
            Ok(Err(_)) => return (false, total),
            Err(_) => {}                    // no frame yet; keep waiting
        }
    }
    (false, total)
}

/// Drain a viewer until its connection is observed closed (clean EOF or error),
/// proving it was dropped rather than fed unboundedly. Returns false if it never
/// closes within `budget` (i.e. it was NOT dropped — a regression).
async fn drain_until_closed(vc: &mut ViewerClient, budget: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < budget {
        match tokio::time::timeout(Duration::from_millis(500), vc.recv()).await {
            Ok(Ok(Some(_))) => {}         // buffered backlog; keep draining
            Ok(Ok(None)) => return true,  // clean EOF -> connection dropped
            Ok(Err(_)) => return true,    // error -> connection dropped
            Err(_) => {}                   // idle; keep waiting within budget
        }
    }
    false
}

#[tokio::test]
async fn slow_viewer_dropped_not_oom() {
    let h = Harness::start().await;
    // A real shell so we can generate a large output flood on demand.
    h.spawn_hub_relay("/bin/sh");
    h.wait_path(&h.paths.sock(SessionId(1))).await;
    let rec = load_record_retry(&h.paths.record(SessionId(1))).await;
    let relay_pid = rec.pid;
    assert!(alive(relay_pid), "relay should be running");

    // Two viewers on the same shell. A reads normally; B connects then STOPS.
    let mut a = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let mut b = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), a.recv()).await; // Replay A
    let _ = tokio::time::timeout(Duration::from_millis(500), b.recv()).await; // Replay B

    // Generate a large flood (>> the ~8 MiB per-viewer bound) ending in a marker
    // built at runtime so it does NOT appear in the echoed command line — this
    // forces A to consume the entire flood before it matches. B never reads, so
    // it overflows its bound and must be dropped.
    a.send_input(b"seq 1 3000000; printf 'ZZ%sZZ\\n' DONE\n").await;

    // (b) Viewer A keeps receiving output correctly through the whole flood.
    let (a_saw_end, a_bytes) = drain_for_marker(&mut a, b"ZZDONEZZ", Duration::from_secs(60)).await;
    assert!(a_saw_end, "viewer A must keep receiving output through the flood (got {a_bytes} bytes)");
    assert!(a_bytes > 8 * 1024 * 1024, "A should have drained the multi-MB flood, got {a_bytes} bytes");

    // (a) Viewer B, too far behind, was disconnected (bounded, not OOM): its
    // socket carries the bounded backlog then a clean EOF.
    assert!(
        drain_until_closed(&mut b, Duration::from_secs(15)).await,
        "slow viewer B must be dropped (connection closed), not grown unbounded"
    );

    // (c) The shell / relay survived a slow viewer being dropped.
    assert!(alive(relay_pid), "shell/relay must keep running after dropping a slow viewer");

    // Prove the relay is still fully usable: a fresh viewer attaches, replays,
    // and round-trips input (the lossless reconnect path a dropped viewer uses).
    let mut c = ViewerClient::connect(&h.paths.daemon_sock(), SessionId(1)).await.unwrap();
    let _ = tokio::time::timeout(Duration::from_millis(500), c.recv()).await; // Replay C
    c.send_input(b"printf 'AF%sTER\\n' TERDROPMARK\n").await;
    let (c_ok, _) = drain_for_marker(&mut c, b"AFTERDROPMARKTER", Duration::from_secs(10)).await;
    assert!(c_ok, "a freshly reconnected viewer must resync and round-trip input");
}
