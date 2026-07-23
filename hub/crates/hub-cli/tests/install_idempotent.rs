use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::{self, Manifest, TouchedFile};
use hub_cli::rcfile::Shell;
use hub_cli::snippet::{BEGIN, BRIDGE_BEGIN};
use std::fs;
use std::path::Path;

fn count_marker(s: &str) -> usize {
    s.lines().filter(|l| l.trim_end() == BEGIN).count()
}

#[test]
fn install_is_idempotent_and_backs_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();
    let after_first = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(count_marker(&after_first), 1, "one block after first install");
    assert!(after_first.contains("export FOO=bar"), "preserves prior content");
    assert!(
        after_first.contains(r#"export PATH="$HOME/.hub/bin:$PATH""#),
        "injected block prepends ~/.hub/bin to PATH"
    );

    // Backup captured original.
    let entry = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let backup = fs::read_to_string(entry.backup.as_ref().unwrap()).unwrap();
    assert_eq!(backup, "export FOO=bar\n");

    // Second install: no double-inject.
    let mut m2 = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m2).unwrap();
    let after_second = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(count_marker(&after_second), 1, "still exactly one block");
    // Idempotent re-run records nothing (block already present).
    assert!(m2.entries.is_empty(), "re-run touches nothing");
}

/// Hardening regression test: the install manifest must be durable on disk
/// as soon as `inject_all` has made its edits, NOT only at the very end of
/// `hub install`'s `run()` (which used to call `manifest::save` exactly
/// once, after autostart). If `run()` crashed between an rc edit and that
/// final save, the on-disk manifest stayed empty while a guarded block +
/// backup already existed on disk — `hub uninstall` would then never find
/// (and thus never revert) that block, and a later `hub install` would see
/// the block already present (`ensure_block` returns None) and never
/// re-record it either: permanently untracked.
///
/// We don't need to literally kill the process mid-install to prove the
/// fix: `inject_all` now persists the manifest to `home`'s manifest path
/// after every single touched-file edit (see `install::persist`), so by
/// asserting the on-disk manifest already reflects the in-memory entries
/// the instant `inject_all` returns — i.e. strictly *before* `run()` would
/// go on to configure autostart or hit its old end-of-function save — we
/// verify that a crash at that point (or anywhere after) is uninstall
/// recoverable.
#[test]
fn install_manifest_persisted_before_end() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();

    create_hub_tree(home).unwrap();
    let manifest_path = home.join(".hub").join("install-manifest.json");

    // Before any edits: no manifest on disk yet.
    assert!(!manifest_path.exists(), "manifest must not exist before install");

    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();

    // This is the crash point the bug report is about: autostart hasn't run
    // yet, and the old code wouldn't call `manifest::save` until after that
    // AND after this whole function returned. The fix must have already
    // written the manifest by now.
    assert!(
        manifest_path.exists(),
        "manifest must be persisted to disk immediately after inject_all, \
         before autostart / end of run()"
    );
    assert!(!m.entries.is_empty(), "inject_all should have touched .zshrc");

    let on_disk = manifest::load(&manifest_path).unwrap();
    assert_eq!(
        on_disk.entries.len(),
        m.entries.len(),
        "on-disk manifest entry count must match in-memory entries right after inject_all"
    );
    for (disk_entry, mem_entry) in on_disk.entries.iter().zip(m.entries.iter()) {
        assert_eq!(disk_entry.path, mem_entry.path);
        assert_eq!(disk_entry.backup, mem_entry.backup);
        assert_eq!(disk_entry.post_install_sha256, mem_entry.post_install_sha256);
        assert_eq!(disk_entry.block, mem_entry.block);
    }

    // The touched rc file + its backup are already on disk too, matching
    // the manifest we just verified — this is exactly the state `hub
    // uninstall` needs to fully revert the install even if we crashed right
    // here.
    let entry = on_disk.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    assert!(Path::new(&entry.backup.clone().unwrap()).exists());
    assert!(home.join(".zshrc").exists());
}

#[test]
fn hub_tree_created_0700() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    create_hub_tree(tmp.path()).unwrap();
    for rel in [".hub", ".hub/sessions", ".hub/logs", ".hub/backups"] {
        let mode = fs::metadata(tmp.path().join(rel)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{rel} must be 0700");
    }
}

#[test]
fn manifest_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(".hub").join("install-manifest.json");

    let mut m = Manifest {
        version: 1,
        ..Default::default()
    };
    m.entries.push(TouchedFile {
        path: "/home/u/.zshrc".to_string(),
        backup: Some("/home/u/.hub/backups/.zshrc.123.bak".to_string()),
        created_by_hub: false,
        post_install_sha256: "deadbeef".to_string(),
        block: "snippet".to_string(),
    });
    m.binaries.push("/usr/local/bin/hub".to_string());
    m.install_prefix = Some("/usr/local/bin".to_string());

    manifest::save(&path, &m).unwrap();
    let loaded = manifest::load(&path).unwrap();

    assert_eq!(loaded.version, 1);
    assert_eq!(loaded.entries.len(), 1);
    assert_eq!(loaded.entries[0].path, "/home/u/.zshrc");
    assert_eq!(
        loaded.entries[0].backup.as_deref(),
        Some("/home/u/.hub/backups/.zshrc.123.bak")
    );
    assert!(!loaded.entries[0].created_by_hub);
    assert_eq!(loaded.entries[0].block, "snippet");
    assert_eq!(loaded.binaries, vec!["/usr/local/bin/hub".to_string()]);
    assert_eq!(loaded.install_prefix.as_deref(), Some("/usr/local/bin"));
}

#[test]
fn load_missing_manifest_yields_empty_v1() {
    let tmp = tempfile::tempdir().unwrap();
    let m = manifest::load(&tmp.path().join("nope.json")).unwrap();
    assert_eq!(m.version, 1);
    assert!(m.entries.is_empty());
    assert!(m.autostart.is_none());
}

/// CreateProfile branch: no .bash_profile / .bash_login, but ~/.profile already
/// sources ~/.bashrc. The generated .bash_profile must preserve ~/.profile WITHOUT
/// adding a second `. ~/.bashrc` (avoid double-source), while remaining idempotent.
#[test]
fn create_profile_avoids_double_source_when_profile_sources_bashrc() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".bashrc"), "export BAR=1\n").unwrap();
    fs::write(
        home.join(".profile"),
        "# my profile\nif [ -f \"$HOME/.bashrc\" ]; then . \"$HOME/.bashrc\"; fi\n",
    )
    .unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m).unwrap();

    let profile = fs::read_to_string(home.join(".bash_profile")).unwrap();
    // Bridge marker present + preserves ~/.profile.
    assert!(profile.lines().any(|l| l.trim_end() == BRIDGE_BEGIN));
    assert!(profile.contains(".profile"), "sources ~/.profile to preserve it");
    // No explicit bashrc source (profile already brings it in) -> no double-source.
    let bashrc_sources = profile
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains(".bashrc"))
        .count();
    assert_eq!(bashrc_sources, 0, "must not re-source ~/.bashrc");

    // Idempotent: re-run adds nothing.
    let before = profile.clone();
    let mut m2 = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m2).unwrap();
    let after = fs::read_to_string(home.join(".bash_profile")).unwrap();
    assert_eq!(before, after, "create-profile re-run is a no-op");
}

/// CreateProfile branch where ~/.profile does NOT source bashrc: generated
/// .bash_profile must source ~/.bashrc so hub loads in login shells.
#[test]
fn create_profile_sources_bashrc_when_profile_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".bashrc"), "export BAR=1\n").unwrap();
    fs::write(home.join(".profile"), "# plain profile, no bashrc\nexport PATH=$PATH\n").unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m).unwrap();

    let profile = fs::read_to_string(home.join(".bash_profile")).unwrap();
    assert!(profile.contains(".profile"), "preserves ~/.profile");
    assert!(
        profile.lines().any(|l| {
            !l.trim_start().starts_with('#') && l.contains(".bashrc") && l.contains('.')
        }),
        "must source ~/.bashrc when profile does not"
    );
}
