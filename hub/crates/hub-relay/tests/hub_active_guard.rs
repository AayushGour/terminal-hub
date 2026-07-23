//! Regression: a relay-spawned shell MUST carry `HUB_ACTIVE=1` so the shell-rc
//! integration snippet's `[ -z "$HUB_ACTIVE" ]` re-exec guard trips and the
//! shell does NOT run `hub attach --new` again on startup.
//!
//! Without it, a Hub-origin session (spawned by the GUI as `hub-relay --origin
//! hub` directly, so the relay process inherits no HUB_ACTIVE) sources the
//! user's rc, the hook re-fires, and it spawns a NESTED External relay whose pty
//! is bridged onto this shell's pty -- the two sessions mirror each other. Both
//! tests here FAIL before the `shell_env` fix (the shell would see an empty
//! `HUB_ACTIVE`) and pass after it.

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
// the relay ALWAYS injects HUB_ACTIVE=1 on top of an empty cfg.env, for every
// origin -- Hub is the one that would otherwise mirror.
#[test]
fn shell_env_always_marks_hub_active() {
    for origin in [hub_proto::Origin::Hub, hub_proto::Origin::External] {
        let env = shell_env(&hub_cfg(origin));
        assert!(
            env.iter().any(|(k, v)| k == "HUB_ACTIVE" && v == "1"),
            "relay-spawned shell ({origin:?}) must carry HUB_ACTIVE=1 to stop the \
             rc hook re-spawning a nested relay (mirror bug); got {env:?}"
        );
    }
}

// End-to-end: a real Hub-origin shell actually spawned by the relay sees
// HUB_ACTIVE=1 in its environment. This exercises the full spawn_pty path
// (that spawn_pty uses shell_env), not just the helper.
#[tokio::test]
async fn spawned_hub_shell_sees_hub_active_set() {
    let cfg = hub_cfg(hub_proto::Origin::Hub);
    let (mut pty, out, _screen) = RelayState::spawn_pty(&cfg).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<RelayEvent>();
    bridge_pty(out, ev_tx);

    // Print the value bracketed so an UNSET var shows as `HA=[]` (the bug) and
    // the injected value shows as `HA=[1]` (the fix). `sh` is POSIX so `printf`
    // and `${HUB_ACTIVE}` are portable.
    pty.write(b"printf 'HA=[%s]\\n' \"$HUB_ACTIVE\"\n").unwrap();

    let mut acc = String::new();
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), ev_rx.recv()).await {
            Ok(Some(RelayEvent::Output(bytes))) => {
                acc.push_str(&String::from_utf8_lossy(&bytes));
                if acc.contains("HA=[1]") {
                    return; // fixed: shell saw HUB_ACTIVE=1
                }
                assert!(
                    !acc.contains("HA=[]"),
                    "relay-spawned Hub shell saw an EMPTY HUB_ACTIVE -- the rc hook \
                     would re-fire and spawn a nested mirroring relay. Output: {acc:?}"
                );
            }
            _ => {}
        }
    }
    panic!("never saw the HUB_ACTIVE probe output; got: {acc:?}");
}
