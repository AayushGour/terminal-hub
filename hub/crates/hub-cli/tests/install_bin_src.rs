// Self-contained install (`hub install --bin-src <DIR>`): copies the three hub
// binaries into `~/.hub/bin`, prepends that dir to PATH inside the marked rc
// block (outside the capture guard), points autostart at the COPIED
// `hub-daemon`, and records the binaries in the manifest — and `hub uninstall`
// reverses ALL of it (rc restored byte-for-byte, `~/.hub` incl `bin/` removed).
//
// Both tests drive the REAL `install::run` / `uninstall::run`. They set
// `HUB_SKIP_SERVICE_ACTIVATION` so the autostart plumbing still writes+removes
// the plist/unit file (keeping the daemon path verifiable) without ever
// shelling out to the host's launchd/systemd, and clear `HUB_DIR`/`HUB_SOCK`
// so `~/.hub` resolves under the throwaway HOME.
use hub_cli::manifest::{self, AutostartEntry};
use hub_cli::paths;
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// Prepare a fresh throwaway HOME + a `bin_src` dir holding three fake, +x
/// binaries named `hub`, `hub-daemon`, `hub-relay`. Also pins the process env
/// (SHELL=zsh for deterministic detection; skip real service activation; no
/// HUB_DIR/HUB_SOCK) so the install lands under this HOME.
fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    std::env::set_var("HUB_SKIP_SERVICE_ACTIVATION", "1");
    std::env::set_var("SHELL", "/bin/zsh");
    std::env::remove_var("HUB_DIR");
    std::env::remove_var("HUB_SOCK");

    let home = tempfile::tempdir().unwrap();
    // A pre-existing rc so install detects zsh and backs up real content.
    fs::write(home.path().join(".zshrc"), "export FOO=bar\n").unwrap();

    let src = tempfile::tempdir().unwrap();
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let p = src.path().join(name);
        fs::write(&p, format!("#!/bin/sh\necho fake-{name}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (home, src)
}

fn autostart_target(entry: &AutostartEntry) -> String {
    // The generated plist/unit embeds the daemon program path; read the file
    // back and return its contents so we can assert what autostart launches.
    match entry {
        AutostartEntry::Launchd { plist, .. } => fs::read_to_string(plist).unwrap(),
        AutostartEntry::Systemd { unit, .. } => fs::read_to_string(unit).unwrap(),
    }
}

#[test]
fn install_bin_src_copies_and_paths() {
    let (home_dir, src) = setup();
    let home = home_dir.path();

    hub_cli::install::run(home, true, Some(src.path())).unwrap();

    // 1. All three binaries copied into ~/.hub/bin at mode 0755.
    let bin = paths::bin_dir(home);
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let dst = bin.join(name);
        assert!(dst.exists(), "{name} must be copied into ~/.hub/bin");
        let mode = fs::metadata(&dst).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755, "{name} must be mode 0755");
    }
    // The bin dir itself is locked down to 0700.
    let bin_mode = fs::metadata(&bin).unwrap().permissions().mode() & 0o777;
    assert_eq!(bin_mode, 0o700, "~/.hub/bin must be 0700");

    // 2. rc block carries BOTH the PATH export and the capture guard.
    let rc = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert!(
        rc.contains(r#"export PATH="$HOME/.hub/bin:$PATH""#),
        "rc block must prepend ~/.hub/bin to PATH"
    );
    assert!(
        rc.contains("hub attach --new && exit"),
        "rc block must keep the guarded capture call"
    );
    assert!(rc.contains("export FOO=bar"), "prior rc content preserved");

    // 3. Manifest lists the three copied binaries + autostart targets the COPY.
    let m = manifest::load(&paths::manifest_path(home)).unwrap();
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let want = bin.join(name).display().to_string();
        assert!(
            m.binaries.contains(&want),
            "manifest must record copied binary {want}; got {:?}",
            m.binaries
        );
    }
    let entry = m.autostart.as_ref().expect("autostart recorded");
    let expected_daemon = bin.join("hub-daemon").display().to_string();
    assert!(
        autostart_target(entry).contains(&expected_daemon),
        "autostart must launch the copied {expected_daemon}"
    );
}

#[test]
fn uninstall_reverts_bin_src_install() {
    let (home_dir, src) = setup();
    let home = home_dir.path();
    let original = fs::read_to_string(home.join(".zshrc")).unwrap();

    hub_cli::install::run(home, true, Some(src.path())).unwrap();
    // Sanity: install actually did the self-contained work.
    assert!(paths::bin_dir(home).join("hub-daemon").exists());
    assert!(fs::read_to_string(home.join(".zshrc")).unwrap().contains(">>> hub"));

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(hub_cli::uninstall::run(home, true, false)).unwrap();

    // rc restored byte-for-byte: no PATH line, no marked block.
    let restored = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(restored, original, "rc must be restored byte-for-byte");
    assert!(!restored.contains(">>> hub"), "no hub block left");
    assert!(!restored.contains(".hub/bin"), "no PATH line left");

    // ~/.hub (including bin/) is gone.
    assert!(!paths::hub_dir(home).exists(), "~/.hub must be removed");
    assert!(
        !paths::bin_dir(home).exists(),
        "~/.hub/bin must be removed with the tree"
    );
}
