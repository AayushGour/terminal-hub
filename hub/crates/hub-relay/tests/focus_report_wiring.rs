//! FIX (move-focus-to-vendor-terminal): before this, an External relay's pty
//! size only ever changed on a real SIGWINCH (the outer terminal's window
//! actually being resized) or on a viewer's `ClaimSize` (e.g. a Hub GUI tile
//! being opened/focused). Switching OS focus back to the vendor terminal
//! without resizing its window sent nothing at all, so the pty stayed pinned
//! at whatever size a Hub tile last claimed instead of snapping back to the
//! vendor terminal's own real size (spec §7 "Focus-follows-size").
//!
//! `FocusReportGuard` asks the outer terminal to emit DECSET 1004 focus
//! events; `strip_focus_reports` is where the relay consumes them off the
//! primary-bridge byte stream. This exercises that function directly against
//! a real pty (so `read_term_size`'s TIOCGWINSZ call is real, not a stub),
//! the same style `sigwinch_wiring.rs` uses for the SIGWINCH path.

use hub_relay::relay::{strip_focus_reports, RelayEvent};
use hub_relay::resize::spawn_coalescer;
use nix::pty::{openpty, Winsize};
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

#[tokio::test]
async fn focus_in_report_is_stripped_and_claims_the_real_size() {
    let ws = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = openpty(Some(&ws), None).expect("openpty");
    let term_fd = pty.master.as_raw_fd();

    let (out, mut rx) = mpsc::unbounded_channel::<RelayEvent>();
    // Session started at the (stale) 80x24 default, as if a Hub tile had
    // claimed it away from the vendor terminal's real 120x40.
    let resize_in = spawn_coalescer(80, 24, out);

    // "hi" + focus-in report + "there", as it would arrive on stdin.
    let mut input = b"hi".to_vec();
    input.extend_from_slice(b"\x1b[I");
    input.extend_from_slice(b"there");

    let forwarded = strip_focus_reports(&input, term_fd, &resize_in);
    assert_eq!(forwarded, b"hithere", "the focus report itself must not reach the inner pty");

    let ev = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await.unwrap().unwrap();
    match ev {
        RelayEvent::ApplyResize(c, r) => assert_eq!((c, r), (120, 40)),
        other => panic!("expected ApplyResize(120,40), got {other:?}"),
    }
}

#[tokio::test]
async fn focus_out_report_is_stripped_without_claiming_a_size() {
    let ws = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = openpty(Some(&ws), None).expect("openpty");
    let term_fd = pty.master.as_raw_fd();

    let (out, mut rx) = mpsc::unbounded_channel::<RelayEvent>();
    let resize_in = spawn_coalescer(80, 24, out);

    let mut input = b"before".to_vec();
    input.extend_from_slice(b"\x1b[O");
    input.extend_from_slice(b"after");

    let forwarded = strip_focus_reports(&input, term_fd, &resize_in);
    assert_eq!(forwarded, b"beforeafter", "the focus-out report must be stripped like focus-in");

    // No claim should have been made -- nothing should arrive on the coalescer.
    let res = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(res.is_err(), "focus-out must not push a resize claim, got {res:?}");
}

#[tokio::test]
async fn plain_bytes_without_focus_reports_pass_through_unchanged() {
    let ws = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = openpty(Some(&ws), None).expect("openpty");
    let term_fd = pty.master.as_raw_fd();

    let (out, mut rx) = mpsc::unbounded_channel::<RelayEvent>();
    let resize_in = spawn_coalescer(80, 24, out);

    let input = b"ls -la\r\n".to_vec();
    let forwarded = strip_focus_reports(&input, term_fd, &resize_in);
    assert_eq!(forwarded, input);

    let res = tokio::time::timeout(std::time::Duration::from_millis(150), rx.recv()).await;
    assert!(res.is_err(), "ordinary keystrokes must not push a resize claim, got {res:?}");
}
