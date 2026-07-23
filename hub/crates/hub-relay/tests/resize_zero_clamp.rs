//! M6: a resize applied to the pty + Screen must never carry a 0 dimension
//! (a race between the coalescer and a teardown, or a plain bad client, could
//! otherwise hand the pty/`vt100::set_size` a 0x0 target). The ApplyResize
//! handler in relay.rs clamps via `clamp_resize` before calling
//! `Pty::resize`/`Screen::resize`. This proves both the clamp math AND that
//! the clamped size is safe to actually hand to a real pty + Screen (no
//! panic), mirroring hub-pty's `resize_reports_new_size_via_stty` pattern.

use hub_relay::relay::clamp_resize;

#[test]
fn clamp_resize_floors_zero_to_one() {
    assert_eq!(clamp_resize(0, 0), (1, 1));
    assert_eq!(clamp_resize(0, 40), (1, 40));
    assert_eq!(clamp_resize(120, 0), (120, 1));
    // Non-degenerate sizes pass through unchanged.
    assert_eq!(clamp_resize(120, 40), (120, 40));
}

#[test]
fn clamped_zero_resize_does_not_panic_pty_or_screen() {
    let (mut pty, out) = hub_pty::Pty::spawn(
        "/bin/sh", ".", &[], hub_pty::PtySize { cols: 80, rows: 24 },
    ).expect("spawn sh");
    let mut screen = hub_term::Screen::new(24, 80, 100);

    // What the ApplyResize handler does when it receives a (0, 0) request.
    let (cols, rows) = clamp_resize(0, 0);
    pty.resize(hub_pty::PtySize { cols, rows }).expect("pty resize must not fail");
    screen.resize(rows, cols); // must not panic (vt100::set_size(1,1) is valid)

    // Confirm the pty actually landed at the clamped 1x1, not 0x0.
    pty.write(b"stty size\n").unwrap();
    let start = std::time::Instant::now();
    let mut seen = String::new();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        if let Ok(chunk) = out.rx.recv_timeout(std::time::Duration::from_millis(200)) {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("1 1") { break; }
        }
    }
    assert!(seen.contains("1 1"), "expected '1 1' (rows cols) from stty size; saw: {seen:?}");
}
