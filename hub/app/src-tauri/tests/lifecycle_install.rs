// Integration proof for the app-lifecycle backend LOGIC (`hub_do_install` /
// `hub_do_uninstall`), driven WITHOUT a Tauri runtime and against a fully
// THROWAWAY `$HOME` so it never touches the developer's real `~/.zshrc`,
// `~/.hub`, launchd, or — critically — any real `.app` bundle.
//
// It exercises the exact pure helpers the Tauri commands delegate to
// (`lifecycle::run_hub_install` / `run_hub_uninstall`) against the REAL built
// `hub` CLI, using a fake bundled-bin dir: the real `hub` binary plus tiny fake
// `hub-daemon` / `hub-relay` stand-ins (the install only copies their bytes;
// autostart is neutered via `HUB_SKIP_SERVICE_ACTIVATION`, so they never run).
//
// The self-delete is deliberately NOT invoked here — `run_hub_uninstall` does
// the reversible teardown only; the bundle self-delete lives in the Tauri
// command and is covered (guard + command shape) by the pure unit tests in
// `src/lifecycle.rs`. So this test can never trash a real bundle.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const HUB_MARKER: &str = "# >>> hub shell integration >>>";

fn built_hub() -> PathBuf {
    // Build the CLI (mirrors tests/real_daemon.rs building relay/daemon).
    assert!(
        std::process::Command::new("cargo")
            .args(["build", "-p", "hub-cli"])
            .status()
            .unwrap()
            .success(),
        "cargo build -p hub-cli failed"
    );
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push("../../target/debug/hub");
    assert!(p.is_file(), "built hub not found at {}", p.display());
    p
}

/// Assemble a fake bundled-bin dir: real `hub` (must be executable to run) plus
/// tiny fake daemon/relay files (install just copies their bytes).
fn make_bin_src(base: &Path, real_hub: &Path) -> PathBuf {
    let dir = base.join("bundled-bin");
    fs::create_dir_all(&dir).unwrap();
    fs::copy(real_hub, dir.join("hub")).unwrap();
    fs::set_permissions(dir.join("hub"), fs::Permissions::from_mode(0o755)).unwrap();
    for fake in ["hub-daemon", "hub-relay"] {
        fs::write(dir.join(fake), b"#!/bin/sh\n# fake hub binary for install test\n").unwrap();
        fs::set_permissions(dir.join(fake), fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

#[test]
fn install_then_uninstall_reverts_everything_in_a_throwaway_home() {
    let real_hub = built_hub();

    let tmp = std::env::temp_dir().join(format!("hub-app-lifecycle-{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let home = tmp.join("home");
    fs::create_dir_all(&home).unwrap();
    // Pre-existing (empty) ~/.zshrc so zsh is detected and there's a backup to
    // restore to on uninstall.
    fs::write(home.join(".zshrc"), b"").unwrap();

    let bin_src = make_bin_src(&tmp, &real_hub);

    // Isolate the child `hub`: HOME -> throwaway; no HUB_DIR/HUB_SOCK leakage;
    // deterministic zsh; and skip the launchctl/systemctl shell-out so the host
    // service manager is never touched (the plist is still written, but under
    // the throwaway HOME). These are inherited by the `hub` subprocess.
    std::env::set_var("HOME", &home);
    std::env::set_var("SHELL", "/bin/zsh");
    std::env::set_var("HUB_SKIP_SERVICE_ACTIVATION", "1");
    std::env::remove_var("HUB_DIR");
    std::env::remove_var("HUB_SOCK");

    // --- install ---
    hub_app_lib::lifecycle::run_hub_install(&bin_src).expect("hub install should succeed");

    let hub_home = home.join(".hub");
    assert!(hub_home.is_dir(), "~/.hub created");
    assert!(
        hub_home.join("install-manifest.json").is_file(),
        "install manifest written"
    );
    assert!(
        hub_home.join("bin").join("hub").is_file(),
        "self-contained hub copied into ~/.hub/bin"
    );
    let rc_after_install = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert!(
        rc_after_install.contains(HUB_MARKER),
        "guarded snippet injected into ~/.zshrc; got:\n{rc_after_install}"
    );

    // --- uninstall (via the installed copy, as production prefers) ---
    let installed_hub = hub_home.join("bin").join("hub");
    hub_app_lib::lifecycle::run_hub_uninstall(&installed_hub).expect("hub uninstall should succeed");

    assert!(!hub_home.exists(), "~/.hub removed by uninstall");
    let rc_after_uninstall = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert!(
        !rc_after_uninstall.contains(HUB_MARKER),
        "snippet removed from ~/.zshrc; got:\n{rc_after_uninstall}"
    );

    // Cleanup.
    std::env::remove_var("HUB_SKIP_SERVICE_ACTIVATION");
    let _ = fs::remove_dir_all(&tmp);
}
