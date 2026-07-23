mod common;

use common::{run_in_pty, TempHome};
use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::Manifest;
use hub_cli::rcfile::Shell;
use hub_cli::snippet::BEGIN;
use hub_cli::uninstall::{restore_file, RestoreOutcome};
use std::time::Duration;

fn install_zsh_snippet(h: &TempHome, original: &str) -> Manifest {
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
    // Leak to get 'static-ish str lifetimes for the harness signature.
    let path: &'static str = Box::leak(path.into_boxed_str());
    let home: &'static str = Box::leak(home.to_string().into_boxed_str());
    let log: &'static str = Box::leak(h.call_log.display().to_string().into_boxed_str());
    let mut v = vec![("HOME", home), ("PATH", path), ("HUB_CALL_LOG", log)];
    v.extend_from_slice(extra);
    v
}

// GATE 1: daemon down → sourcing rc still yields a working plain shell.
#[test]
fn gate_daemon_down_yields_working_shell() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("FAKE_HUB_EXIT", "1")]); // 1 = daemon unreachable
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            // Marker split by empty quotes so the pty's INPUT echo never contains
            // the contiguous string "HUB_TEST_OK" — only the shell's real output
            // (emitted strictly AFTER `source` runs the snippet) matches
            // reached_prompt(). Otherwise the read loop would break on the echoed
            // keystrokes and kill the child before it ever invoked hub.
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must stay usable when daemon is down");
    assert!(
        h.calls().contains("attach --new"),
        "snippet should have attempted attach"
    );
}

// GATE 2: HUB_DISABLE=1 → snippet fully bypassed.
#[test]
fn gate_hub_disable_bypasses_snippet() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("HUB_DISABLE", "1")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt());
    assert!(
        !h.calls().contains("attach --new"),
        "HUB_DISABLE=1 must skip hub entirely"
    );
}

// GATE 3: re-exec guard — HUB_ACTIVE=1 does not recurse.
#[test]
fn gate_hub_active_does_not_recurse() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("HUB_ACTIVE", "1")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt());
    assert!(
        !h.calls().contains("attach --new"),
        "inside a hub session the snippet must not recurse"
    );
}

// GATE 4: non-interactive (no tty) → snippet bypassed.
#[test]
fn gate_non_interactive_bypasses() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    // Plain piped bash (no pty) → `[ -t 1 ]` false.
    let out = std::process::Command::new("bash")
        .args(["--norc", "-c"])
        .arg(format!(
            "source {}/.bashrc; echo HUB_TEST_OK",
            h.home().display()
        ))
        .env("HOME", h.home())
        .env("PATH", format!("{}:{}", h.bin.display(), std::env::var("PATH").unwrap()))
        .env("HUB_CALL_LOG", &h.call_log)
        .env("FAKE_HUB_EXIT", "1")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("HUB_TEST_OK"));
    assert!(
        !h.calls().contains("attach --new"),
        "non-interactive shells must skip the snippet"
    );
}

// GATE (PATH): the `export PATH="$HOME/.hub/bin:$PATH"` line lives OUTSIDE the
// interactive/capture guard, so sourcing the rc must prepend ~/.hub/bin to PATH
// even in a non-interactive shell with HUB_DISABLE=1 (every capture guard off).
// This is what lets the snippet's own `command -v hub` and `hub attach --new`'s
// sibling `hub-relay` lookup resolve against the self-contained ~/.hub/bin.
#[test]
fn gate_path_export_runs_outside_capture_guard() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    // Piped (no tty) bash with HUB_DISABLE=1: the capture guard cannot fire, so
    // only the unguarded PATH export can affect $PATH.
    let out = std::process::Command::new("bash")
        .args(["--norc", "-c"])
        .arg(format!(
            "source {}/.bashrc; printf '%s\\n' \"$PATH\"",
            h.home().display()
        ))
        .env("HOME", h.home())
        .env("PATH", std::env::var("PATH").unwrap())
        .env("HUB_CALL_LOG", &h.call_log)
        .env("HUB_DISABLE", "1")
        .output()
        .unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert!(
        printed.contains("/.hub/bin"),
        "sourcing the rc must prepend ~/.hub/bin to PATH even with the capture guard bypassed; \
         got PATH: {printed}"
    );
    assert!(
        !h.calls().contains("attach --new"),
        "capture guard must still be bypassed (HUB_DISABLE + non-interactive)"
    );
}

// GATE 5: no double-inject — install twice → snippet appears exactly once.
#[test]
fn gate_no_double_inject() {
    let h = TempHome::new();
    let mut m = install_zsh_snippet(&h, "export FOO=bar\n");
    inject_all(h.home(), &[Shell::Zsh], &mut m).unwrap(); // second run
    let content = std::fs::read_to_string(h.home().join(".zshrc")).unwrap();
    let count = content.lines().filter(|l| l.trim_end() == BEGIN).count();
    assert_eq!(count, 1, "exactly one hub block after two installs");
}

// GATE 6: uninstall restores the rc file byte-for-byte.
#[test]
fn gate_uninstall_restores_byte_for_byte() {
    let h = TempHome::new();
    let original = "export FOO=bar\nalias g=git\n";
    let m = install_zsh_snippet(&h, original);
    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::RestoredBackup));
    assert_eq!(
        std::fs::read_to_string(h.home().join(".zshrc")).unwrap(),
        original
    );
}

// GATE 1 (zsh variant): daemon down → sourcing rc still yields a working shell.
#[test]
fn gate_daemon_down_yields_working_shell_zsh() {
    if std::process::Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh not installed; skipping");
        return;
    }
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("FAKE_HUB_EXIT", "1"), ("ZDOTDIR", h.home().to_str().unwrap())]);
    let probe = run_in_pty(
        "zsh",
        &["-i"],
        &env,
        &[
            &format!("source {}/.zshrc", h.home().display()),
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "zsh must stay usable when daemon is down");
    assert!(h.calls().contains("attach --new"));
}

// GATE 7 (the non-negotiable proof): daemon down → real production behavior is
// that `hub attach` *execs a fall-through plain shell* carrying HUB_ACTIVE=1
// (attach::exec_shell). This gate installs a fake `hub` that faithfully mimics
// that: it logs, then `exec`s a fresh interactive shell WITH HUB_ACTIVE=1. The
// re-launched shell auto-sources the SAME rc, whose snippet must see HUB_ACTIVE
// and NOT re-attach. If the guard were broken, `hub` would be re-invoked on
// every re-source, nesting forever. We PROVE the recursion is bounded three
// ways: (a) the fake hub self-caps at 4 invocations and prints a loud marker if
// it is ever called more than once, (b) the whole run is bounded by a 10s
// timeout (a genuine infinite loop would blow it and fail the assert, not pass),
// and (c) we assert `hub` was invoked EXACTLY ONCE and the nesting marker never
// appeared. A usable prompt (HUB_TEST_OK) must also be reached.
#[test]
fn gate_no_infinite_nesting_when_hub_execs_shell() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");

    // Fake `hub`: mimic production exec_shell — log, self-cap the recursion so a
    // broken guard fails fast instead of hanging, then exec a fresh interactive
    // shell carrying HUB_ACTIVE=1 (exactly what attach::exec_shell does).
    h.set_fake_hub(concat!(
        "#!/bin/sh\n",
        "printf '%s\\n' \"$*\" >> \"$HUB_CALL_LOG\"\n",
        "n=$(wc -l < \"$HUB_CALL_LOG\")\n",
        "if [ \"$n\" -ge 4 ]; then\n",
        "  printf 'HUB_NESTING_DETECTED\\n'\n",
        "  exit 0\n",
        "fi\n",
        "HUB_ACTIVE=1 exec \"$FAKE_SHELL\" -i\n",
    ));

    let env = bash_env(
        &h,
        &[("FAKE_HUB_EXIT", "1"), ("FAKE_SHELL", "/bin/bash")],
    );
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            // Triggers the snippet → fake hub → exec bash -i (HUB_ACTIVE=1) →
            // that shell auto-sources ~/.bashrc → snippet sees HUB_ACTIVE → skips.
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST''_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );

    assert!(
        probe.reached_prompt(),
        "shell must reach a usable prompt after the fall-through exec"
    );
    assert!(
        !probe.output.contains("HUB_NESTING_DETECTED"),
        "hub must NOT be re-invoked by the re-sourced rc (infinite nesting)"
    );
    let attach_calls = h.calls().matches("attach --new").count();
    assert_eq!(
        attach_calls, 1,
        "hub attach must run EXACTLY once; >1 means the HUB_ACTIVE re-exec guard \
         failed and the terminal would nest forever (got {attach_calls} calls)"
    );
}
