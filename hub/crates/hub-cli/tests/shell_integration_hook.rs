// Tests for the OSC 7 / OSC 133 shell-integration hook added to the SAME
// managed rc block as the existing attach/gate logic (see
// docs/superpowers/specs/2026-07-23-shell-integration-design.md, section 4).
//
// Follows the exact "spawn a real pty shell, source the injected rc, drive
// commands, inspect raw stdout bytes" pattern already established by
// rc_gate.rs -- the hook only fires when HUB_ACTIVE=1 (the INNER, captured
// shell hub-relay spawns), which is the OPPOSITE guard from the existing
// `hub attach --new` block (which only fires in the OUTER, uncaptured login
// shell where HUB_ACTIVE is unset).

mod common;

use common::{run_in_pty, TempHome};
use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::Manifest;
use hub_cli::rcfile::Shell;
use std::time::Duration;

const OSC_A: &str = "\x1b]133;A\x07";
const OSC_C: &str = "\x1b]133;C\x07";
const OSC_D0: &str = "\x1b]133;D;0\x07";
const OSC_D1: &str = "\x1b]133;D;1\x07";
const OSC7_PREFIX: &str = "\x1b]7;file://";

fn install(h: &TempHome, original: &str) -> Manifest {
    std::fs::write(h.home().join(".zshrc"), original).unwrap();
    std::fs::write(h.home().join(".bashrc"), original).unwrap();
    create_hub_tree(h.home()).unwrap();
    let mut m = Manifest::default();
    inject_all(h.home(), &[Shell::Zsh, Shell::Bash], &mut m).unwrap();
    m
}

fn bash_env<'a>(h: &'a TempHome, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let home = h.home().to_str().unwrap();
    let path = format!("{}:{}", h.bin.display(), std::env::var("PATH").unwrap());
    let path: &'static str = Box::leak(path.into_boxed_str());
    let home: &'static str = Box::leak(home.to_string().into_boxed_str());
    let log: &'static str = Box::leak(h.call_log.display().to_string().into_boxed_str());
    let mut v = vec![("HOME", home), ("PATH", path), ("HUB_CALL_LOG", log)];
    v.extend_from_slice(extra);
    v
}

/// Index of the first occurrence of `needle` in `haystack`, panicking with a
/// readable message (incl. hex dump) if absent -- makes failures debuggable
/// since the interesting bytes are non-printing.
fn find(haystack: &str, needle: &str, what: &str) -> usize {
    haystack.find(needle).unwrap_or_else(|| {
        panic!(
            "expected {what} ({:x?}) not found in output.\nRaw output bytes: {:x?}\nOutput (lossy): {haystack:?}",
            needle.as_bytes(),
            haystack.as_bytes(),
        )
    })
}

// HUB_ACTIVE=1 (inner captured shell): zsh must emit OSC 7 with the correct
// $PWD, OSC 133;A at each prompt, OSC 133;C when a command starts, and
// OSC 133;D;<exit_code> with the RIGHT exit code once it finishes -- for both
// a successful (`true`, 0) and failing (`false`, 1) command.
#[test]
fn zsh_active_emits_osc_sequences_with_correct_exit_codes() {
    if std::process::Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh not installed; skipping");
        return;
    }
    let h = TempHome::new();
    install(&h, "export FOO=bar\n");
    let env = bash_env(
        &h,
        &[("HUB_ACTIVE", "1"), ("ZDOTDIR", h.home().to_str().unwrap())],
    );
    let probe = run_in_pty(
        "zsh",
        &["-i"],
        &env,
        &[
            &format!("source {}/.zshrc", h.home().display()),
            "true",
            "false",
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must reach a usable prompt");
    assert!(
        !h.calls().contains("attach --new"),
        "HUB_ACTIVE=1 must never trigger a re-attach"
    );

    let out = &probe.output;
    let cwd_idx = find(out, OSC7_PREFIX, "OSC 7 prefix");
    // The path portion after `file://<host>` must contain the real $PWD (the
    // temp HOME dir sourcing happened in).
    let home = h.home().to_str().unwrap();
    assert!(
        out[cwd_idx..].contains(home),
        "OSC 7 payload must contain the real cwd {home:?}; got: {out:?}"
    );
    find(out, OSC_A, "OSC 133;A (prompt displayed)");
    let c_idx = find(out, OSC_C, "OSC 133;C (command started)");
    let d0_idx = find(out, OSC_D0, "OSC 133;D;0 (true's exit code)");
    let d1_idx = find(out, OSC_D1, "OSC 133;D;1 (false's exit code)");
    assert!(c_idx < d0_idx, "C for `true` must precede its own D;0");
    assert!(d0_idx < d1_idx, "D;0 (true) must precede D;1 (false)");
}

// Same contract, bash side: PROMPT_COMMAND + trap DEBUG must not misfire on
// its own internals, and $? must be captured before it's clobbered.
#[test]
fn bash_active_emits_osc_sequences_with_correct_exit_codes() {
    let h = TempHome::new();
    install(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("HUB_ACTIVE", "1")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "true",
            "false",
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must reach a usable prompt");
    assert!(
        !h.calls().contains("attach --new"),
        "HUB_ACTIVE=1 must never trigger a re-attach"
    );

    let out = &probe.output;
    let cwd_idx = find(out, OSC7_PREFIX, "OSC 7 prefix");
    let home = h.home().to_str().unwrap();
    assert!(
        out[cwd_idx..].contains(home),
        "OSC 7 payload must contain the real cwd {home:?}; got: {out:?}"
    );
    find(out, OSC_A, "OSC 133;A (prompt displayed)");
    let c_idx = find(out, OSC_C, "OSC 133;C (command started)");
    let d0_idx = find(out, OSC_D0, "OSC 133;D;0 (true's exit code)");
    let d1_idx = find(out, OSC_D1, "OSC 133;D;1 (false's exit code)");
    assert!(c_idx < d0_idx, "C for `true` must precede its own D;0");
    assert!(d0_idx < d1_idx, "D;0 (true) must precede D;1 (false)");
}

// HUB_ACTIVE unset (outer, uncaptured login shell): the hook must stay
// completely silent -- no OSC 7/133 bytes at all -- while the existing,
// unrelated `hub attach --new` gate logic in the SAME block is left
// completely untouched by this edit.
//
// NOTE on scope: this deliberately does NOT re-assert
// `h.calls().contains("attach --new")` here. That exact scenario (daemon
// down, HUB_ACTIVE unset) is already covered by
// `rc_gate::gate_daemon_down_yields_working_shell[_zsh]`, which are two of
// the three tests independently confirmed to already fail on a clean,
// unmodified tree (pre-existing/unrelated bug -- see task notes). Baseline
// vs. post-edit `cargo test -p hub-cli --test rc_gate` runs are byte-for-byte
// identical (same 3 failing, same 6 passing, same panic messages), which is
// the actual proof this edit left that logic untouched -- reasserting the
// same broken invariant here would just make this file inherit that known
// flake for no added signal. What IS asserted below is squarely this file's
// own scope: the new hook must be silent when HUB_ACTIVE is unset, and the
// shell must still reach a usable prompt.
#[test]
fn zsh_inactive_stays_silent() {
    if std::process::Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh not installed; skipping");
        return;
    }
    let h = TempHome::new();
    install(&h, "export FOO=bar\n");
    // HUB_ACTIVE="" (explicit, empty): forces the "outer, uncaptured" scenario
    // even if the ambient environment this test suite itself runs in already
    // has HUB_ACTIVE=1 set (true on this machine, since portable_pty's
    // CommandBuilder inherits the full parent env and common::run_in_pty
    // never clears it -- an empty-but-set value satisfies the SAME
    // `${HUB_ACTIVE:-}` unset-or-null semantics as truly unsetting it, for
    // both this block's `[ -n ... ]` guard and the existing block's
    // `[ -z ... ]` guard).
    let env = bash_env(
        &h,
        &[
            ("FAKE_HUB_EXIT", "1"),
            ("HUB_ACTIVE", ""),
            ("ZDOTDIR", h.home().to_str().unwrap()),
        ],
    );
    let probe = run_in_pty(
        "zsh",
        &["-i"],
        &env,
        &[
            &format!("source {}/.zshrc", h.home().display()),
            "true",
            "false",
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must stay usable when daemon is down");
    assert!(
        !probe.output.contains("\x1b]133;"),
        "OSC 133 must never fire outside HUB_ACTIVE; got: {:?}",
        probe.output
    );
    assert!(
        !probe.output.contains("\x1b]7;"),
        "OSC 7 must never fire outside HUB_ACTIVE; got: {:?}",
        probe.output
    );
}

#[test]
fn bash_inactive_stays_silent() {
    let h = TempHome::new();
    install(&h, "export FOO=bar\n");
    // See the matching comment in zsh_inactive_stays_silent: HUB_ACTIVE="" is
    // an explicit override so this test is deterministic regardless of
    // whatever the ambient environment running this suite already has set
    // (this machine's own shell has HUB_ACTIVE=1 for real, since it's itself
    // a captured hub session -- portable_pty inherits full parent env).
    let env = bash_env(&h, &[("FAKE_HUB_EXIT", "1"), ("HUB_ACTIVE", "")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "true",
            "false",
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must stay usable when daemon is down");
    assert!(
        !probe.output.contains("\x1b]133;"),
        "OSC 133 must never fire outside HUB_ACTIVE; got: {:?}",
        probe.output
    );
    assert!(
        !probe.output.contains("\x1b]7;"),
        "OSC 7 must never fire outside HUB_ACTIVE; got: {:?}",
        probe.output
    );
}
