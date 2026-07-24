//! OSC 7 (cwd) / OSC 133 (command lifecycle) scanner.
//!
//! Design spec: `docs/superpowers/specs/2026-07-23-shell-integration-design.md`
//! §3 (wire formats), §5 (this module), §7 (performance).
//!
//! A minimal `vte::Perform` implementation that ONLY reacts to `osc_dispatch`
//! for codes `7` and `133` -- every other callback (print/execute/hook/put/
//! unhook/csi_dispatch/esc_dispatch) is left at `vte::Perform`'s default
//! no-op impl, so this never builds a grid, tracks cursor position, or
//! allocates a screen buffer. It costs roughly one incremental tokenization
//! pass over the same bytes `Screen::feed` already processes (spec §7) --
//! proportional to actual output volume, ~0 for an idle session, nowhere near
//! double the cost of `Screen::feed`'s full grid/scrollback maintenance.
//!
//! Deliberately NOT inside `Screen` and does not touch the vendored `vt100`
//! crate (spec §5) -- a separate, sibling parser fed the exact same bytes.

/// One recognized shell-integration event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShellEvent {
    /// OSC 7: the shell's cwd, absolute path only (host stripped -- see
    /// `parse_osc7`).
    Cwd(String),
    /// OSC 133;D;<exit_code>: a command just finished.
    CommandFinished(i32),
}

/// `vte::Perform` sink: collects `ShellEvent`s as they're recognized. Every
/// callback besides `osc_dispatch` is left at the trait's default no-op, so
/// print/execute/csi/esc bytes are ignored entirely, as required.
#[derive(Default)]
struct EventSink {
    events: Vec<ShellEvent>,
}

impl vte::Perform for EventSink {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        match params.first().copied() {
            Some(b"7") => {
                if let Some(path) = params.get(1).and_then(|p| parse_osc7(p)) {
                    self.events.push(ShellEvent::Cwd(path));
                }
            }
            Some(b"133") => match params.get(1).copied() {
                Some(b"D") => {
                    if let Some(code) = params.get(2).and_then(|p| parse_exit_code(p)) {
                        self.events.push(ShellEvent::CommandFinished(code));
                    }
                }
                // A (prompt displayed) / C (command started): recognized and
                // silently consumed -- kept in the wire format for forward
                // compatibility (spec §3) but no handler logic yet.
                Some(b"A") | Some(b"C") => {}
                _ => {}
            },
            // Any other OSC code (e.g. 0/2 window title, already handled by
            // `vt100` separately) is not ours -- ignored.
            _ => {}
        }
    }
}

/// `ESC ] 7 ; file://<host><path> BEL` -- parsing rule (frozen, spec §3):
/// after `file://`, the FIRST `/` onward (inclusive) is the absolute path.
/// The hostname is ignored entirely (not validated/matched).
fn parse_osc7(bytes: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(bytes).ok()?;
    let rest = s.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    Some(rest[slash..].to_string())
}

/// `<exit_code>` is an ASCII decimal integer (spec §3), e.g. `0`, `1`, `127`.
fn parse_exit_code(bytes: &[u8]) -> Option<i32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Incremental OSC 7/133 scanner. Feed it the exact same bytes as
/// `Screen::feed` gets in the relay's pty-output path -- it's a lightweight
/// sibling pass, not a replacement (spec §5/§7).
pub struct ShellIntegration {
    parser: vte::Parser,
    sink: EventSink,
}

impl Default for ShellIntegration {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellIntegration {
    pub fn new() -> Self {
        ShellIntegration { parser: vte::Parser::new(), sink: EventSink::default() }
    }

    /// Feed a chunk of raw pty-output bytes; returns any `ShellEvent`s
    /// recognized while processing THIS chunk. `vte::Parser` is incremental
    /// (byte-at-a-time state machine), so an OSC sequence split across two
    /// `feed()` calls -- pty output arrives in arbitrary chunks -- is handled
    /// correctly: the event fires on whichever call completes the sequence.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<ShellEvent> {
        for &b in bytes {
            self.parser.advance(&mut self.sink, b);
        }
        std::mem::take(&mut self.sink.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc7_extracts_absolute_path_and_ignores_host() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]7;file://myhost/Users/aayush/projects/terminal-hub\x07");
        assert_eq!(ev, vec![ShellEvent::Cwd("/Users/aayush/projects/terminal-hub".into())]);
    }

    #[test]
    fn osc7_empty_host_still_parses() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]7;file:///Users/a\x07");
        assert_eq!(ev, vec![ShellEvent::Cwd("/Users/a".into())]);
    }

    #[test]
    fn osc7_split_across_two_feed_calls_still_fires() {
        // pty output arrives in arbitrary chunks; `vte::Parser` must carry
        // state across `feed()` calls and only report the event once the
        // sequence is actually complete.
        let mut si = ShellIntegration::new();
        let whole = b"\x1b]7;file://host/a/b/c\x07";
        let mid = whole.len() / 2;
        let first = si.feed(&whole[..mid]);
        assert!(first.is_empty(), "must not fire before the sequence is complete");
        let second = si.feed(&whole[mid..]);
        assert_eq!(second, vec![ShellEvent::Cwd("/a/b/c".into())]);
    }

    #[test]
    fn osc133_d_reports_exit_code() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]133;D;127\x07");
        assert_eq!(ev, vec![ShellEvent::CommandFinished(127)]);
    }

    #[test]
    fn osc133_d_split_across_two_feed_calls_still_fires() {
        let mut si = ShellIntegration::new();
        let whole = b"\x1b]133;D;255\x07";
        let mid = whole.len() / 2;
        assert!(si.feed(&whole[..mid]).is_empty());
        assert_eq!(si.feed(&whole[mid..]), vec![ShellEvent::CommandFinished(255)]);
    }

    #[test]
    fn osc133_a_and_c_are_recognized_and_silently_consumed() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]133;A\x07\x1b]133;C\x07");
        assert!(ev.is_empty(), "A/C must be recognized+consumed with no ShellEvent (spec §3)");
    }

    #[test]
    fn osc0_and_osc2_window_title_produce_no_shell_event() {
        // Must not be misread as 7/133 by this scanner. `hub-term`'s own
        // `Screen` tests (lib.rs) separately confirm vt100's OSC 0/2 title
        // parsing keeps working, unaffected by this sibling module.
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]0;icon+title\x07\x1b]2;title only\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn interleaved_plain_text_and_osc_sequences_only_yield_recognized_events() {
        let mut si = ShellIntegration::new();
        let mut all = Vec::new();
        all.extend_from_slice(b"$ ls\r\n");
        all.extend_from_slice(b"\x1b]133;C\x07");
        all.extend_from_slice(b"file1  file2\r\n");
        all.extend_from_slice(b"\x1b]133;D;0\x07");
        all.extend_from_slice(b"\x1b]7;file://host/home/u\x07");
        all.extend_from_slice(b"\x1b]0;my shell\x07");
        all.extend_from_slice(b"\x1b]133;A\x07");
        all.extend_from_slice(b"$ ");
        let ev = si.feed(&all);
        assert_eq!(
            ev,
            vec![ShellEvent::CommandFinished(0), ShellEvent::Cwd("/home/u".into())]
        );
    }

    #[test]
    fn malformed_osc7_without_file_scheme_produces_no_event() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]7;not-a-file-uri\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn malformed_osc133_d_without_exit_code_produces_no_event() {
        let mut si = ShellIntegration::new();
        let ev = si.feed(b"\x1b]133;D\x07");
        assert!(ev.is_empty());
    }

    #[test]
    fn feed_returns_only_events_from_the_latest_chunk() {
        // Calling `feed` again after a batch of events must not re-report
        // stale ones -- `feed` drains its internal buffer each call.
        let mut si = ShellIntegration::new();
        let first = si.feed(b"\x1b]133;D;1\x07");
        assert_eq!(first, vec![ShellEvent::CommandFinished(1)]);
        let second = si.feed(b"plain text, no OSC here");
        assert!(second.is_empty());
    }
}
