// `hub update` (post-install binary swap + safe daemon restart), proven
// against REAL `hub-daemon`/`hub-relay` binaries -- not fakes -- because the
// entire point of this command is the daemon-restart choreography
// (`update::run`'s module doc): stop the OLD daemon process, swap the binary
// on disk out from under it, start a NEW daemon process, and prove a live
// session survives, untouched, across that restart.
//
// Modeled on `hub-daemon/tests/teardown_origin.rs`'s real-binary harness
// (build real bins, spawn against a throwaway `HUB_DIR`, poll for
// sockets/liveness rather than sleeping a fixed duration) and
// `hub-cli/tests/install_bin_src.rs`'s pattern of driving the real
// `hub_cli::*::run` entry points end-to-end.
use hub_cli::{daemon_client, manifest, paths, update};
use hub_proto::SessionId;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::sync::Mutex;

// `HUB_DIR`/`HUB_SOCK`/`SHELL` are process-global env vars; serialize every
// test in this file on this lock for its full body so the two tests below
// (which each point `HUB_DIR` at their own throwaway dir) can't race each
// other if `cargo test` schedules them on parallel threads. Mirrors
// `hub-cli/src/paths.rs`'s own `ENV_LOCK` test convention, using the
// async-aware `tokio::sync::Mutex` (not `std::sync::Mutex`) since both tests
// hold the guard across `.await` points.
static ENV_LOCK: Mutex<()> = Mutex::const_new(());

fn daemon_bin() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-daemon");
    p
}
fn relay_bin() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub-relay");
    p
}
fn hub_bin() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub");
    p
}
fn build() {
    assert!(std::process::Command::new("cargo")
        .args(["build", "-p", "hub-cli", "-p", "hub-daemon", "-p", "hub-relay"])
        .status()
        .unwrap()
        .success());
}

async fn wait_sock(p: &std::path::Path) {
    for _ in 0..300 {
        if p.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

/// Poll `list_sessions` for the given id to (re)appear -- used after the
/// restart to confirm the new daemon process re-adopted the still-running
/// relay. Bounded, not a fixed sleep, matching every other wait in this file
/// and in `update.rs` itself.
async fn wait_session_listed(sock: &std::path::Path, id: SessionId) -> bool {
    for _ in 0..200 {
        if let Ok(sessions) = daemon_client::list_sessions(sock).await {
            if sessions.iter().any(|s| s.id == id) {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

/// Seed an "already installed" `HUB_DIR`: copies of the three REAL just-built
/// binaries under `<dir>/bin`, plus a manifest recording them -- the minimal
/// on-disk state `hub update` requires as a precondition, without going
/// through the full (rc-file-editing, shell-detecting) `hub install` flow
/// that isn't relevant to what this test is proving.
fn seed_installed(dir: &std::path::Path) {
    let bin_dir = paths::bin_dir(dir);
    fs::create_dir_all(&bin_dir).unwrap();
    let mut m = manifest::Manifest::default();
    for (name, real) in [
        ("hub", hub_bin()),
        ("hub-daemon", daemon_bin()),
        ("hub-relay", relay_bin()),
    ] {
        let dst = bin_dir.join(name);
        fs::copy(&real, &dst).unwrap();
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
        m.binaries.push(dst.display().to_string());
    }
    manifest::save(&paths::manifest_path(dir), &m).unwrap();
}

#[tokio::test]
async fn update_replaces_binaries_and_preserves_live_session() {
    let _g = ENV_LOCK.lock().await;
    build();
    std::env::set_var("HUB_SKIP_SERVICE_ACTIVATION", "1");
    std::env::remove_var("HUB_SOCK");

    let dir = std::env::temp_dir().join(format!("hubupd-ok-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    std::env::set_var("HUB_DIR", &dir);

    seed_installed(&dir);
    let bin_dir = paths::bin_dir(&dir);
    let sock = paths::daemon_sock_path(&dir);

    // Start the OLD daemon process from the seeded install copy (exactly the
    // binary `hub update` is about to swap out from under it).
    let mut old_daemon = std::process::Command::new(bin_dir.join("hub-daemon"))
        .env("HUB_DIR", &dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_sock(&sock).await;

    // A real, long-lived, external-origin relay session with its stdin held
    // open (standing in for the outer terminal) -- exactly the shape of
    // session `hub update` must never disrupt.
    let mut relay = std::process::Command::new(bin_dir.join("hub-relay"))
        .args([
            "--origin", "external", "--shell", "/bin/cat", "--size", "80x24",
            "--daemon-sock", sock.to_str().unwrap(),
        ])
        .env("HUB_DIR", &dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let relay_pid = relay.id();
    wait_sock(&paths::sessions_dir(&dir).join("1.sock")).await;
    assert!(alive(relay_pid), "relay must be running before update");
    assert!(
        wait_session_listed(&sock, SessionId(1)).await,
        "session must be visible via the OLD daemon before update"
    );

    // A fresh `bin_src` (same real binaries, different dir) so `update::run`
    // exercises the real `install::copy_binaries` path, not a no-op.
    let src = tempfile::tempdir().unwrap();
    for (name, real) in [
        ("hub", hub_bin()),
        ("hub-daemon", daemon_bin()),
        ("hub-relay", relay_bin()),
    ] {
        let dst = src.path().join(name);
        fs::copy(&real, &dst).unwrap();
        fs::set_permissions(&dst, fs::Permissions::from_mode(0o755)).unwrap();
    }

    update::run(&dir, true, Some(src.path()), None)
        .await
        .expect("hub update must succeed against a real, live install");

    // (a) Binaries actually replaced on disk: still present, mode 0755, and
    // byte-identical to the `bin_src` copy `update::run` just copied from
    // (proves a real copy happened, not a skipped no-op).
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let dst = bin_dir.join(name);
        assert!(dst.exists(), "{name} must still exist after update");
        let mode = fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "{name} must remain mode 0755");
        assert_eq!(
            fs::read(&dst).unwrap(),
            fs::read(src.path().join(name)).unwrap(),
            "{name} on disk must match the bin_src copy"
        );
    }

    // (b) Manifest still records all three binaries (idempotent, not
    // duplicated) after the update.
    let m = manifest::load(&paths::manifest_path(&dir)).unwrap();
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let want = bin_dir.join(name).display().to_string();
        assert_eq!(
            m.binaries.iter().filter(|b| *b == &want).count(),
            1,
            "{name} must be recorded exactly once in the manifest"
        );
    }

    // The OLD daemon process must have actually been stopped (a real
    // restart happened, not two daemons running in parallel).
    let mut old_gone = false;
    for _ in 0..200 {
        match old_daemon.try_wait() {
            Ok(Some(_)) => {
                old_gone = true;
                break;
            }
            Ok(None) => {}
            Err(_) => {
                old_gone = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(old_gone, "old daemon process must have exited during the update");

    // (c) THE critical safety property: the relay (and the shell/pty it
    // owns) must still be alive, and the NEW daemon process must have
    // re-adopted the session -- both without ever touching the relay.
    assert!(alive(relay_pid), "relay must survive the daemon restart");
    assert!(
        wait_session_listed(&sock, SessionId(1)).await,
        "session must be re-adopted and visible again via the NEW daemon"
    );

    // Cleanup: stop the newly-spawned daemon gracefully (it's the same RPC
    // this whole feature relies on), then tear down the relay by closing its
    // held-open stdin (mirrors `hub-daemon/tests/teardown_origin.rs`).
    let _ = daemon_client::shutdown_daemon(&sock).await;
    drop(relay.stdin.take());
    let _ = relay.wait();
    let _ = fs::remove_dir_all(&dir);
    std::env::remove_var("HUB_DIR");
}

#[tokio::test]
async fn update_fails_clearly_when_not_installed() {
    let _g = ENV_LOCK.lock().await;
    std::env::remove_var("HUB_SOCK");

    let dir = std::env::temp_dir().join(format!("hubupd-noinst-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    std::env::set_var("HUB_DIR", &dir);

    // No manifest, no binaries -- `hub update` must refuse cleanly, not
    // panic and not silently no-op.
    let err = update::run(&dir, true, None, None)
        .await
        .expect_err("update must fail when hub was never installed");
    assert!(
        format!("{err:#}").contains("not installed"),
        "error must clearly say hub isn't installed, got: {err:#}"
    );

    let _ = fs::remove_dir_all(&dir);
    std::env::remove_var("HUB_DIR");
}
