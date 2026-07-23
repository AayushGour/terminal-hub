//! hub-term: headless vt100 screen + replay snapshot.

/// A headless terminal screen backed by `vt100`, used to build REPLAY snapshots.
pub struct Screen {
    parser: vt100::Parser,
}

impl Screen {
    /// `scrollback` = max scrollback lines (default 10_000, configurable later).
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Screen {
        Screen {
            parser: vt100::Parser::new(rows, cols, scrollback),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        // vt100 0.15: Parser::set_size. (0.16+: Screen::set_size — see executor note.)
        self.parser.set_size(rows, cols);
    }

    /// ANSI byte stream that reproduces the CURRENT screen when written to a
    /// fresh terminal. Used for REPLAY on attach.
    pub fn replay_bytes(&self) -> Vec<u8> {
        self.parser.screen().contents_formatted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feed_then_replay_contains_visible_text() {
        let mut screen = Screen::new(24, 80, 10_000);
        screen.feed(b"hello world");
        let replay = screen.replay_bytes();
        // contents_formatted embeds escape codes but the literal glyphs survive.
        let as_str = String::from_utf8_lossy(&replay);
        assert!(as_str.contains("hello world"), "replay missing text: {as_str:?}");
    }

    #[test]
    fn replay_reproduces_screen_in_a_fresh_terminal() {
        let mut a = Screen::new(10, 40, 100);
        a.feed(b"line one\r\nline two\r\n");
        let replay = a.replay_bytes();

        let mut b = Screen::new(10, 40, 100);
        b.feed(&replay);
        // Re-rendering A's replay into B yields the identical current screen.
        assert_eq!(a.replay_bytes(), b.replay_bytes());
    }

    #[test]
    fn resize_does_not_panic_and_new_width_takes_effect() {
        let mut screen = Screen::new(24, 80, 100);
        screen.feed(b"before resize");
        screen.resize(10, 20);
        screen.feed(b"\r\nafter resize");
        let replay = screen.replay_bytes();
        assert!(String::from_utf8_lossy(&replay).contains("after resize"));
    }

    #[test]
    fn scrollback_cap_keeps_latest_lines_on_screen() {
        // 3 visible rows, small scrollback; write far more lines than fit.
        // Each write is "rowN\r\n", so the cursor always ends on a fresh blank
        // line after the last completed row; with 3 rows that leaves the two
        // most recently completed lines (row48, row49) visible above it.
        let mut screen = Screen::new(3, 20, 3);
        for i in 0..50 {
            screen.feed(format!("row{i}\r\n").as_bytes());
        }
        let visible = String::from_utf8_lossy(&screen.replay_bytes()).to_string();
        // Only the last two written lines (row49, row48) remain on screen;
        // everything older has scrolled off entirely (this screen has no
        // scrollback query API, so "gone" means "not on screen").
        assert!(visible.contains("row49"), "latest line should be on screen");
        assert!(visible.contains("row48"), "second-latest line should be on screen");
        // "row0" / "row40" are not substrings of "row48"/"row49" (or of any
        // other still-visible row), so their absence genuinely discriminates
        // evicted lines from a broken implementation that left them on screen.
        assert!(!visible.contains("row0"), "oldest line (row0) should be gone: {visible:?}");
        assert!(!visible.contains("row40"), "line row40 should be gone: {visible:?}");
    }
}
