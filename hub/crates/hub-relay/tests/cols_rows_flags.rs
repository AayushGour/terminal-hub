//! I1 regression: `hub attach --new` (hub-cli) sends the real terminal size
//! as `--cols <n> --rows <n>` (see hub-cli/src/attach.rs::plan_attach), NOT
//! `--size <WxH>`. hub-relay's arg parser used to only understand `--size`,
//! silently dropping --cols/--rows into the catch-all -> every externally
//! captured session was pinned at the 80x24 default regardless of the real
//! terminal. This spawns the REAL compiled `hub-relay` binary (so it goes
//! through main.rs's parser, not just the library) with --cols 120 --rows 40
//! and asserts the session record it writes reflects 120x40. Fails against
//! the old parser (record would show the 80x24 default).

use hub_proto::{Origin, SessionId};
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

fn target_bin(name: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(format!("../../target/debug/{name}"));
    p
}

fn relay_bin() -> std::path::PathBuf { target_bin("hub-relay") }
fn daemon_bin() -> std::path::PathBuf { target_bin("hub-daemon") }

/// Kills the child on drop so a failing assertion (which unwinds past a
/// plain `Child`) can't leak a daemon/relay process across test runs.
struct KillOnDrop(Child);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..200 {
        if p.exists() { return; }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_file(p: &std::path::Path) -> bool {
    for _ in 0..300 {
        if p.exists() { return true; }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

#[tokio::test]
async fn relay_binary_honors_cols_rows_flags_from_attach() {
    // Build both real bins first so the paths exist (mirrors detach_reparent.rs).
    let ok = Command::new("cargo").args(["build", "-p", "hub-daemon", "-p", "hub-relay"]).status().unwrap().success();
    assert!(ok, "build hub-daemon + hub-relay");

    let dir = std::env::temp_dir().join(format!("colsrows-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir.clone());
    paths.ensure_dirs().unwrap();

    // Real, separate-process daemon (hub-relay cannot depend on hub-daemon,
    // so unlike focus_size.rs this can't be driven in-process here).
    let daemon = KillOnDrop(
        Command::new(daemon_bin())
            .env("HUB_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn hub-daemon"),
    );
    wait_sock(&paths.daemon_sock()).await;

    // Spawn the REAL hub-relay binary exactly as hub-cli's attach.rs does:
    // --cols/--rows, never --size. Keep stdin open (piped, not closed) so
    // the External primary bridge doesn't see an immediate EOF and tear the
    // session down before we can inspect it.
    let relay = KillOnDrop(
        Command::new(relay_bin())
            .args([
                "--origin", "external",
                "--shell", "/bin/sh",
                "--cwd", ".",
                "--term", "xterm-256color",
                "--cols", "120",
                "--rows", "40",
            ])
            .env("HUB_DIR", &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn hub-relay"),
    );

    // Session ids start at 1 (mirrors focus_size.rs's SessionId(1) assumption).
    let record_path = paths.record(SessionId(1));
    let found = wait_file(&record_path).await;
    assert!(found, "relay never wrote a session record");

    let rec = SessionRecord::load(&record_path).expect("load session record");
    assert_eq!(rec.origin, Origin::External);
    assert_eq!((rec.cols, rec.rows), (120, 40), "record must reflect --cols/--rows, not the 80x24 default");

    drop(relay);
    drop(daemon);
}
