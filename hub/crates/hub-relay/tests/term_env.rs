//! Regression: a relay-spawned shell MUST see `TERM=<cfg.term>` explicitly.
//!
//! `portable_pty::CommandBuilder` doesn't clear the environment before
//! spawning, so without this the child shell falls back to whatever ambient
//! `TERM` the `hub-relay` PROCESS itself happened to inherit. For
//! External-origin (launched from inside a real terminal via `hub attach`)
//! that's a harmless accident -- it inherits the real terminal's own correct
//! `TERM`. For Hub-origin (spawned directly by the GUI app, which has no
//! controlling terminal) `TERM` ends up unset or stale, so zsh picks a
//! terminfo entry that doesn't match what xterm.js actually emulates --
//! visible as corrupted, overlapping redraws on every keystroke (ZLE/prompt
//! plugins issue capability-dependent cursor-movement escapes xterm.js then
//! misinterprets). Both tests here FAIL before the `shell_env` fix (the shell
//! sees an empty `TERM`) and pass after it.

use hub_relay::relay::{bridge_pty, shell_env, RelayConfig, RelayEvent, RelayState};
use tokio::sync::mpsc;

fn hub_cfg(origin: hub_proto::Origin) -> RelayConfig {
    RelayConfig {
        shell: "/bin/sh".into(),
        cwd: "/".into(),
        env: vec![], // exactly how the CLI builds it (hub-relay/src/main.rs)
        cols: 80,
        rows: 24,
        term: "xterm-256color".into(),
        origin,
        title: "t".into(),
    }
}

// Pure, deterministic (no dependence on the test process's own environment):
// the relay ALWAYS injects TERM=cfg.term on top of an empty cfg.env, for every
// origin -- Hub is the one that would otherwise inherit nothing.
#[test]
fn shell_env_always_sets_term_from_cfg() {
    for origin in [hub_proto::Origin::Hub, hub_proto::Origin::External] {
        let mut cfg = hub_cfg(origin);
        cfg.term = "xterm-256color".into();
        let env = shell_env(&cfg);
        assert!(
            env.iter().any(|(k, v)| k == "TERM" && v == "xterm-256color"),
            "relay-spawned shell ({origin:?}) must carry TERM=<cfg.term> so zsh/ZLE \
             picks a terminfo entry matching what xterm.js actually emulates; got {env:?}"
        );
    }
}

// End-to-end: a real Hub-origin shell actually spawned by the relay sees
// TERM=xterm-256color in its environment. This exercises the full spawn_pty
// path (that spawn_pty uses shell_env), not just the helper.
#[tokio::test]
async fn spawned_hub_shell_sees_term_set() {
    let cfg = hub_cfg(hub_proto::Origin::Hub);
    let (mut pty, out, _screen) = RelayState::spawn_pty(&cfg).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<RelayEvent>();
    bridge_pty(out, ev_tx);

    // Print the value bracketed so an UNSET/wrong var shows as `T=[]` or
    // `T=[dumb]` etc (the bug) and the injected value shows as
    // `T=[xterm-256color]` (the fix). `sh` is POSIX so `printf` and
    // `${TERM}` are portable.
    pty.write(b"printf 'T=[%s]\\n' \"$TERM\"\n").unwrap();

    let mut acc = String::new();
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), ev_rx.recv()).await {
            Ok(Some(RelayEvent::Output(bytes))) => {
                acc.push_str(&String::from_utf8_lossy(&bytes));
                if acc.contains("T=[xterm-256color]") {
                    return; // fixed: shell saw the correct TERM
                }
            }
            _ => {}
        }
    }
    panic!("never saw TERM=[xterm-256color] in shell output; got: {acc:?}");
}
