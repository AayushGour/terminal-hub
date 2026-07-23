use hub_cli::attach::{plan_attach, AttachAction, AttachInputs};
use std::path::PathBuf;

fn base() -> AttachInputs {
    AttachInputs {
        shell: "/bin/zsh".into(),
        cwd: "/home/u".into(),
        term: "xterm-256color".into(),
        cols: 120,
        rows: 40,
        hub_active: false,
        relay_path: Some(PathBuf::from("/opt/hub/hub-relay")),
        daemon_sock: PathBuf::from("/home/u/.hub/hubd.sock"),
        daemon_up: true,
    }
}

#[test]
fn daemon_down_falls_through_to_shell() {
    let i = AttachInputs { daemon_up: false, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(s) if s == "/bin/zsh"));
}

#[test]
fn already_active_falls_through_to_shell() {
    let i = AttachInputs { hub_active: true, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(s) if s == "/bin/zsh"));
}

#[test]
fn missing_relay_binary_falls_through_to_shell() {
    let i = AttachInputs { relay_path: None, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(_)));
}

#[test]
fn healthy_path_execs_relay_with_external_origin() {
    match plan_attach(&base()) {
        AttachAction::ExecRelay { relay, args, env } => {
            assert_eq!(relay, PathBuf::from("/opt/hub/hub-relay"));
            assert!(args.windows(2).any(|w| w == ["--origin", "external"]));
            assert!(args.windows(2).any(|w| w == ["--shell", "/bin/zsh"]));
            assert!(args.windows(2).any(|w| w == ["--cwd", "/home/u"]));
            assert!(args.windows(2).any(|w| w == ["--term", "xterm-256color"]));
            assert!(args.windows(2).any(|w| w == ["--cols", "120"]));
            assert!(args.windows(2).any(|w| w == ["--rows", "40"]));
            assert!(args
                .windows(2)
                .any(|w| w == ["--daemon-sock", "/home/u/.hub/hubd.sock"]));
            assert!(env.iter().any(|(k, v)| k == "HUB_ACTIVE" && v == "1"));
        }
        other => panic!("expected ExecRelay, got {other:?}"),
    }
}
