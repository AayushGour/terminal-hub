//! FIX SIGWINCH: for an External-origin relay, the outer terminal is the
//! relay's own controlling terminal, so a SIGWINCH on the relay process
//! means the OUTER terminal changed size and the inner pty/shell must learn
//! the new size too. A real end-to-end test would need to actually resize
//! this test process's own controlling terminal, which isn't practical to
//! drive deterministically in CI/sandboxed test runs (no real tty, and
//! `cargo test` doesn't give us one to resize).
//!
//! Instead this exercises the two real, non-fake pieces that the SIGWINCH
//! handler in relay.rs::spawn_sigwinch_watcher chains together:
//!   1. `read_term_size` — the actual TIOCGWINSZ ioctl helper, run against a
//!      REAL pty (via nix::pty::openpty) sized to a known value, proving it
//!      reads back the true kernel-reported size (not a stub).
//!   2. The resize debounce/apply path (`spawn_coalescer`) that the SIGWINCH
//!      watcher feeds into — the SAME path Attach/ClaimSize/Resize use — is
//!      driven with that real size and must emit `RelayEvent::ApplyResize`
//!      with those exact dimensions.
//!
//! Manual verification of the real SIGWINCH signal delivery: run
//! `hub-relay --origin external --shell /bin/sh --daemon-sock <sock>` in an
//! actual terminal, attach a client, then resize the terminal window (drag
//! the window edge, or `printf '\e[8;40;120t'` in some terminals) and
//! confirm a full-screen program (e.g. `vim`, `htop`) run inside the session
//! reflows to the new size instead of staying pinned at the spawn size.

use hub_relay::relay::{read_term_size, RelayEvent};
use hub_relay::resize::spawn_coalescer;
use nix::pty::{openpty, Winsize};
use std::os::fd::AsRawFd;
use tokio::sync::mpsc;

#[test]
fn read_term_size_reads_real_kernel_winsize() {
    let ws = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = openpty(Some(&ws), None).expect("openpty");

    let got = read_term_size(pty.master.as_raw_fd());
    assert_eq!(got, Some((120, 40)), "TIOCGWINSZ must report the real pty size");
}

#[test]
fn read_term_size_none_on_non_tty_fd() {
    // A plain pipe has no window size: ioctl(TIOCGWINSZ) fails with ENOTTY.
    // The helper must degrade to `None`, never panic.
    let (r, _w) = nix::unistd::pipe().expect("pipe");
    let got = read_term_size(r.as_raw_fd());
    assert_eq!(got, None);
}

/// Simulates exactly what `spawn_sigwinch_watcher` does on each SIGWINCH:
/// read the real terminal size, then push it into the SAME debounced
/// resize-apply path used by Attach/ClaimSize/Resize.
#[tokio::test]
async fn sigwinch_style_size_read_flows_through_resize_debounce() {
    let ws = Winsize { ws_row: 40, ws_col: 120, ws_xpixel: 0, ws_ypixel: 0 };
    let pty = openpty(Some(&ws), None).expect("openpty");

    let (out, mut rx) = mpsc::unbounded_channel::<RelayEvent>();
    // Session started at the (stale) 80x24 default, as if externally captured.
    let resize_in = spawn_coalescer(80, 24, out);

    // What the SIGWINCH handler does: read the real outer-terminal size...
    let (cols, rows) = read_term_size(pty.master.as_raw_fd()).expect("real size");
    // ...and push it into the coalescer, same as ClaimSize/Resize frames do.
    resize_in.send((cols, rows)).unwrap();

    let ev = tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
        .await.unwrap().unwrap();
    match ev {
        RelayEvent::ApplyResize(c, r) => assert_eq!((c, r), (120, 40)),
        other => panic!("expected ApplyResize(120,40), got {other:?}"),
    }
}
