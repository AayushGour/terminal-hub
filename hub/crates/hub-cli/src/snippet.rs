pub const BEGIN: &str = "# >>> hub shell integration >>>";
pub const END: &str = "# <<< hub shell integration <<<";
pub const BRIDGE_BEGIN: &str = "# >>> hub bash_profile bridge >>>";
pub const BRIDGE_END: &str = "# <<< hub bash_profile bridge <<<";

pub const ZSH: &str = include_str!("../../../install/zsh-snippet.sh");
pub const BASH: &str = include_str!("../../../install/bash-snippet.sh");
pub const BASH_PROFILE_BRIDGE: &str = include_str!("../../../install/bash-profile-bridge.sh");

pub fn contains_block(content: &str, begin: &str) -> bool {
    content.lines().any(|l| l.trim_end() == begin)
}

/// Remove the inclusive `begin..=end` marked block (and one trailing blank line
/// if present). Idempotent; leaves all other lines untouched.
/// SAFETY: Only removes the block if BOTH begin and end markers are found.
/// If begin is found but no matching end follows, returns content unchanged
/// (prevents data loss from corrupted/unmatched markers).
pub fn remove_block(content: &str, begin: &str, end: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();

    // First pass: locate BEGIN marker
    let begin_idx = lines.iter().position(|l| l.trim_end() == begin);

    // Second pass: locate END marker at or after BEGIN
    let end_idx = if let Some(begin_pos) = begin_idx {
        lines[begin_pos..]
            .iter()
            .position(|l| l.trim_end() == end)
            .map(|pos| begin_pos + pos)
    } else {
        None
    };

    // SAFETY: If either BEGIN or END is missing, return content unchanged.
    // This prevents data loss from corrupted rc files with unmatched markers.
    if begin_idx.is_none() || end_idx.is_none() {
        return content.to_string();
    }

    let begin_pos = begin_idx.unwrap();
    let end_pos = end_idx.unwrap();

    // Build output, skipping the block range [begin_pos..=end_pos]
    let mut out: Vec<&str> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        if i >= begin_pos && i <= end_pos {
            // Skip the block
            if i == begin_pos {
                // Drop a single preceding blank separator line if we added one.
                if out.last().map(|l: &&str| l.is_empty()).unwrap_or(false) {
                    out.pop();
                }
            }
            continue;
        }
        out.push(line);
    }

    let mut s = out.join("\n");
    if content.ends_with('\n') && !s.is_empty() {
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_start_and_end_with_declared_markers() {
        assert!(ZSH.trim_start().starts_with(BEGIN));
        assert!(ZSH.trim_end().ends_with(END));
        assert!(BASH.trim_start().starts_with(BEGIN));
        assert!(BASH_PROFILE_BRIDGE.trim_start().starts_with(BRIDGE_BEGIN));
    }

    #[test]
    fn blocks_carry_all_five_guards() {
        for block in [ZSH, BASH] {
            assert!(block.contains("HUB_ACTIVE"), "re-exec guard");
            assert!(block.contains("HUB_DISABLE"), "bypass guard");
            assert!(block.contains("[ -t 1 ]"), "tty guard");
            assert!(block.contains("*i*"), "interactive guard");
            assert!(block.contains("command -v hub"), "stale-binary guard");
            assert!(block.contains("hub attach --new"), "attach call");
            assert!(!block.contains("exec hub attach"), "must NOT exec in rc");
            // `&& exit`: close the terminal on a clean session end (relay exits 0
            // on exit OR kill-from-hub) instead of resurrecting an uncaptured
            // login shell -- but only on success, so a non-zero (couldn't start)
            // leaves this login shell running (fail-safe).
            assert!(block.contains("hub attach --new && exit"), "exit on clean session end");
        }
    }

    /// The self-contained PATH export must live INSIDE the marked block (so
    /// `remove_block`/uninstall takes it away with everything else) but OUTSIDE
    /// the interactive/capture guard (so `~/.hub/bin` is discoverable — making
    /// the block's own `command -v hub` and `hub attach --new`'s sibling
    /// `hub-relay` lookup resolve — and it stays a harmless prepend in
    /// non-interactive shells).
    #[test]
    fn blocks_export_path_outside_the_guard() {
        for block in [ZSH, BASH] {
            assert!(
                block.contains(r#"export PATH="$HOME/.hub/bin:$PATH""#),
                "block must prepend ~/.hub/bin to PATH"
            );
            let lines: Vec<&str> = block.lines().collect();
            let path_idx = lines
                .iter()
                .position(|l| l.contains(r#"export PATH="$HOME/.hub/bin"#))
                .expect("PATH export line present");
            let guard_idx = lines
                .iter()
                .position(|l| l.trim_start().starts_with("if [ -z"))
                .expect("capture guard present");
            let begin_idx = lines.iter().position(|l| l.trim_end() == BEGIN).unwrap();
            let end_idx = lines.iter().position(|l| l.trim_end() == END).unwrap();
            assert!(
                begin_idx < path_idx && path_idx < guard_idx && guard_idx < end_idx,
                "PATH export must sit between BEGIN and the guard (inside the block, \
                 outside the guard)"
            );
        }
    }

    #[test]
    fn remove_block_is_exact_inverse_of_append() {
        let original = "line one\nline two\n";
        let injected = format!("{original}\n{ZSH}\n");
        let restored = remove_block(&injected, BEGIN, END);
        // Removing the block leaves the pre-existing content (trailing whitespace trimmed to original).
        assert!(!contains_block(&restored, BEGIN));
        assert!(restored.starts_with("line one\nline two"));
    }

    #[test]
    fn remove_block_leaves_unmatched_begin_untouched() {
        // Regression test: corrupted/unmatched BEGIN marker must not cause data loss.
        // If BEGIN exists but no matching END, the entire content should be returned unchanged.
        let original = "line one\nline two\n# >>> hub shell integration >>>\nline three\nline four\n";
        let restored = remove_block(&original, BEGIN, END);
        assert_eq!(
            restored, original,
            "remove_block must leave content unchanged if BEGIN exists without matching END"
        );
    }
}
