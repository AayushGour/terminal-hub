//! Full pipeline test (design spec 2026-07-23-shell-integration-design.md §5):
//! a shell's OSC 7/133 bytes -> the relay's `ShellIntegration` scanner ->
//! `ControlMsg::SessionActivity` over the relay's REAL persistent daemon
//! connection -> `server::drive_relay` -> `Registry::update_activity` ->
//! visible in a fresh `ControlMsg::List` (the same in-memory registry the
//! GUI's "healthy" bucket sources from -- spec §5's closing paragraph, NOT
//! the disk record). Also confirms the relay's own on-disk `SessionRecord`
//! is rewritten to match.
//!
//! Uses a REAL daemon (`hub_daemon::server::run`) and a REAL relay
//! (`hub_relay::relay::run_relay`) wrapping `/bin/cat`, same pattern as
//! `register_records.rs`/`attach_stream.rs`: `cat` echoes whatever we send
//! it as input back out its pty (real pty ECHO, not a stub), which is how
//! synthetic OSC bytes reach the actor's `RelayEvent::Output` path exactly
//! like real shell-hook output would.

use hub_proto::{encode_control, encode_data, ControlMsg, Frame, Origin, SessionId};
use hub_relay::conn::write_frame;
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 {
        if p.exists() { return; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn shell_osc_sequences_propagate_to_daemon_registry_and_disk_record() {
    let dir = std::env::temp_dir().join(format!("shellact-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();
    std::env::set_var("HUB_DIR", dir.to_str().unwrap());

    let p2 = paths.clone();
    tokio::spawn(async move { hub_daemon::server::run(p2).await.unwrap() });
    wait_sock(&paths.daemon_sock()).await;

    let cfg = hub_relay::relay::RelayConfig {
        shell: "/bin/cat".into(), cwd: "/".into(), env: vec![],
        cols: 80, rows: 24, term: "xterm".into(),
        origin: Origin::External, title: "s".into(),
    };
    let ds = paths.daemon_sock().to_string_lossy().to_string();
    // In-process relay: it does NOT own the test process's stdin, so no primary bridge.
    tokio::spawn(async move { let _ = hub_relay::relay::run_relay(cfg, Some(ds), false).await; });
    wait_sock(&paths.sock(SessionId(1))).await;

    // Attach so `cat`'s echo of our synthetic OSC bytes actually streams
    // (Output only flows to the daemon while >=1 viewer is attached).
    let (mut fr, mut wr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
    write_frame(&mut wr, &encode_control(&ControlMsg::Attach { id: SessionId(1) })).await.unwrap();
    let mut got_replay = false;
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_millis(200), fr.next()).await {
            Ok(Ok(Some(Frame::Control(ControlMsg::Replay { .. })))) => { got_replay = true; break; }
            Ok(Ok(Some(_))) => {}
            _ => {}
        }
    }
    assert!(got_replay, "attach must yield a Replay");

    // Feed synthetic OSC 7 + 133;D bytes as "input", terminated by a
    // newline so `cat` reads a complete line and rewrites it as its OWN
    // output. That rewrite carries the real, unmangled ESC/BEL bytes --
    // unlike the pty's immediate kernel ECHO of the same input, which (with
    // ECHOCTL, the macOS/BSD/Linux default) rewrites control bytes into
    // human-readable caret notation ("\x1b" -> literal "^["), so the FIRST
    // (echoed) copy is plain, harmless text to the scanner and only cat's
    // OWN rewritten copy actually parses as OSC 7/133. Either way this
    // lands on the relay's `RelayEvent::Output` path exactly like real
    // shell-hook output would.
    let osc = b"\x1b]7;file://myhost/srv/app\x07\x1b]133;D;42\x07\n".to_vec();
    write_frame(&mut wr, &encode_data(SessionId(1), &osc)).await.unwrap();

    // Poll a FRESH connection's `ControlMsg::List` (the daemon's live,
    // in-memory registry -- not the disk record) until it reflects both
    // fields, proving `ControlMsg::SessionActivity` made it all the way
    // through `server::drive_relay` -> `Registry::update_activity`.
    let mut ok = false;
    for _ in 0..150 {
        let (mut lfr, mut lwr) = hub_relay::conn::dial_hello(&paths.daemon_sock(), paths.base()).await.unwrap();
        write_frame(&mut lwr, &encode_control(&ControlMsg::List)).await.unwrap();
        if let Ok(Some(Frame::Control(ControlMsg::Sessions { sessions }))) = lfr.next().await {
            if let Some(s) = sessions.iter().find(|s| s.id == SessionId(1)) {
                if s.cwd == "/srv/app" && s.last_exit_code == Some(42) && s.activity_seq == 1 {
                    ok = true;
                    break;
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(ok, "daemon's live registry should reflect the shell-integration activity");

    // The relay's own on-disk record must also have been rewritten to match
    // (so a crashed relay's ghost record still shows the last-known state).
    let mut rec_ok = false;
    for _ in 0..50 {
        if let Ok(rec) = SessionRecord::load(&paths.record(SessionId(1))) {
            if rec.cwd == "/srv/app" && rec.last_exit_code == Some(42) && rec.activity_seq == 1 {
                rec_ok = true;
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(rec_ok, "on-disk SessionRecord should reflect the shell-integration activity");
}
