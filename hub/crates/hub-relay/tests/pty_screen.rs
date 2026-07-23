use hub_relay::relay::{bridge_pty, RelayConfig, RelayEvent, RelayState};
use tokio::sync::mpsc;

#[tokio::test]
async fn pty_output_feeds_screen_and_replay() {
    let cfg = RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm-256color".into(),
        origin: hub_proto::Origin::External, title: "t".into(),
    };
    let (mut pty, out, mut screen) = RelayState::spawn_pty(&cfg).unwrap();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<RelayEvent>();
    bridge_pty(out, ev_tx);

    // Write to the pty; `cat` echoes it back as output.
    pty.write(b"hello\n").unwrap();

    // Drain output events, feed the screen, until we see the echo.
    let mut saw = false;
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), ev_rx.recv()).await {
            Ok(Some(RelayEvent::Output(bytes))) => {
                screen.feed(&bytes);
                if String::from_utf8_lossy(&screen.replay_bytes()).contains("hello") { saw = true; break; }
            }
            _ => {}
        }
    }
    assert!(saw, "screen replay should contain echoed 'hello'");
}
