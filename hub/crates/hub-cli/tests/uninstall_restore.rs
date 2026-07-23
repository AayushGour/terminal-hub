use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::{Manifest, TouchedFile};
use hub_cli::rcfile::Shell;
use hub_cli::uninstall::{plan_dry_run, restore_file, RestoreOutcome};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn uninstall_restores_zshrc_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let original = "export FOO=bar\nalias ll='ls -la'\n";
    fs::write(home.join(".zshrc"), original).unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();
    // File now contains the block.
    assert!(fs::read_to_string(home.join(".zshrc")).unwrap().contains(">>> hub"));

    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::RestoredBackup));

    let restored = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(restored, original, "byte-for-byte restore");
}

#[test]
fn user_edited_file_is_surgically_cleaned_not_clobbered() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();

    // Simulate a post-install user edit.
    let mut c = fs::read_to_string(home.join(".zshrc")).unwrap();
    c.push_str("export ADDED_LATER=1\n");
    fs::write(home.join(".zshrc"), &c).unwrap();

    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));

    let cleaned = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert!(!cleaned.contains(">>> hub"), "hub block gone");
    assert!(cleaned.contains("export FOO=bar"), "kept original");
    assert!(cleaned.contains("export ADDED_LATER=1"), "kept user edit");
}

#[test]
fn created_file_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // No .bashrc/.bash_profile → install CREATES ~/.bash_profile bridge.
    fs::write(home.join(".bashrc"), "").unwrap(); // primary exists, bridge created
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m).unwrap();

    let created = m.entries.iter().find(|e| e.created_by_hub);
    if let Some(t) = created {
        let path = t.path.clone();
        let outcome = restore_file(t).unwrap();
        assert!(matches!(outcome, RestoreOutcome::Deleted));
        assert!(!std::path::Path::new(&path).exists());
    }
}

// --- Extra safety tests required by the task (beyond the brief's three) ---

/// SAFETY: backup missing AND no marker block present → never guess/truncate.
/// The user's file content must survive verbatim.
#[test]
fn missing_backup_and_no_marker_leaves_file_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let rc = home.join(".zshrc");
    let user_content = "export FOO=bar\nalias ll='ls -la'\n# no hub marker here\n";
    fs::write(&rc, user_content).unwrap();

    // A manifest entry whose backup is gone and whose recorded post-install
    // hash no longer matches (forces the surgical fallback path).
    let t = TouchedFile {
        path: rc.display().to_string(),
        backup: None,
        created_by_hub: false,
        post_install_sha256: "0".repeat(64), // deliberately not the current hash
        block: "snippet".to_string(),
    };

    let outcome = restore_file(&t).unwrap();
    // No marker was present, so remove_block is a no-op: content is preserved.
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));
    let after = fs::read_to_string(&rc).unwrap();
    assert_eq!(after, user_content, "file left byte-for-byte untouched");
}

/// SAFETY: a truncated/corrupted rc with an unmatched BEGIN marker but a stale
/// backup that no longer matches must NOT be clobbered by remove_block.
#[test]
fn unmatched_begin_marker_is_left_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let rc = home.join(".zshrc");
    // BEGIN present, no END → remove_block must leave content unchanged.
    let content = "line one\n# >>> hub shell integration >>>\nline three\nline four\n";
    fs::write(&rc, content).unwrap();

    let t = TouchedFile {
        path: rc.display().to_string(),
        backup: None,
        created_by_hub: false,
        post_install_sha256: "0".repeat(64),
        block: "snippet".to_string(),
    };

    let outcome = restore_file(&t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));
    assert_eq!(
        fs::read_to_string(&rc).unwrap(),
        content,
        "unmatched-marker file must be preserved verbatim"
    );
}

/// A manifest entry whose file is already gone reports Missing and mutates nothing.
#[test]
fn missing_file_reports_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let t = TouchedFile {
        path: home.join(".does-not-exist").display().to_string(),
        backup: None,
        created_by_hub: false,
        post_install_sha256: "0".repeat(64),
        block: "snippet".to_string(),
    };
    assert!(matches!(restore_file(&t).unwrap(), RestoreOutcome::Missing));
}

/// SAFETY: `--dry-run` is a pure read-only preview. Nothing on disk changes.
#[test]
fn dry_run_mutates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let original = "export FOO=bar\n";
    fs::write(home.join(".zshrc"), original).unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();
    hub_cli::manifest::save(&hub_cli::paths::manifest_path(home), &m).unwrap();

    // Snapshot everything the uninstaller could touch.
    let zshrc_before = fs::read_to_string(home.join(".zshrc")).unwrap();
    let manifest_before = fs::read_to_string(hub_cli::paths::manifest_path(home)).unwrap();
    let hub_dir = hub_cli::paths::hub_dir(home);
    assert!(hub_dir.exists());
    let backup_paths: Vec<String> = m
        .entries
        .iter()
        .filter_map(|e| e.backup.clone())
        .collect();

    // Exercise the real dry-run entry point.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(hub_cli::uninstall::run(home, true, true)).unwrap();

    // Nothing changed.
    assert_eq!(fs::read_to_string(home.join(".zshrc")).unwrap(), zshrc_before);
    assert_eq!(
        fs::read_to_string(hub_cli::paths::manifest_path(home)).unwrap(),
        manifest_before
    );
    assert!(hub_dir.exists(), "~/.hub still present after dry-run");
    for b in &backup_paths {
        assert!(std::path::Path::new(b).exists(), "backup preserved: {b}");
    }
}

/// C1 regression: a non-UTF-8 rc file with NO hub marker must never be
/// rewritten. Before the fix, `String::from_utf8_lossy` replaced the invalid
/// byte with U+FFFD, made the "cleaned" string differ from the raw bytes even
/// though nothing hub-related was present, and triggered a corrupting
/// rewrite. The byte-safe surgical path must leave it completely untouched.
#[test]
fn surgical_leaves_non_utf8_no_marker_file_untouched() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let rc = home.join(".zshrc");
    let content: &[u8] = b"# user config\n\xFF\xFE stray\n";
    fs::write(&rc, content).unwrap();

    // Backup missing AND hash mismatched forces the surgical fallback path.
    let t = TouchedFile {
        path: rc.display().to_string(),
        backup: None,
        created_by_hub: false,
        post_install_sha256: "0".repeat(64),
        block: "snippet".to_string(),
    };

    let outcome = restore_file(&t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));
    let after = fs::read(&rc).unwrap();
    assert_eq!(
        after, content,
        "non-UTF-8 file with no hub marker must be byte-for-byte unchanged"
    );
}

/// I2 regression: a `created_by_hub` file (e.g. a hub-created ~/.bash_profile
/// bridge) that the user has since added their own content to must NOT be
/// wholesale-deleted. Only hub's own marked block may be removed; the user's
/// lines must survive.
#[test]
fn created_file_with_user_edits_is_not_wholesale_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // No .bashrc/.bash_profile/.bash_login → bash install creates a
    // ~/.bash_profile bridge (`created_by_hub = true`).
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m).unwrap();

    let created = m
        .entries
        .iter()
        .find(|e| e.created_by_hub)
        .expect("bash install with no bashrc/profile should create a bridge file")
        .clone();
    let path = std::path::PathBuf::from(&created.path);

    // Simulate the user editing the hub-created file after install: they add
    // their own content outside hub's marked block.
    let mut c = fs::read_to_string(&path).unwrap();
    c.push_str("\n# my own stuff\nexport MY_VAR=1\n");
    fs::write(&path, &c).unwrap();

    let outcome = restore_file(&created).unwrap();
    assert!(
        !matches!(outcome, RestoreOutcome::Deleted),
        "must not wholesale-delete a created file that gained user content: {outcome:?}"
    );
    assert!(path.exists(), "file with surviving user content must still exist");
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.contains("export MY_VAR=1"), "user content must survive");
    assert!(!after.contains(">>> hub"), "hub's own block should be removed");
}

/// M4 regression: atomic writes (restore/cleanup) must preserve the target
/// file's existing permission bits instead of resetting them to the process
/// default via `File::create`.
#[test]
fn atomic_write_preserves_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();
    fs::set_permissions(home.join(".zshrc"), fs::Permissions::from_mode(0o600)).unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();

    // `ensure_block` in install.rs runs an atomic write to inject the
    // snippet; confirm the mode survived that edit.
    let mode_after_install = fs::metadata(home.join(".zshrc")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_after_install, 0o600, "install's atomic_write must preserve mode");

    // Now drive an edit through uninstall's atomic writer (surgical cleanup)
    // and confirm the mode still survives.
    let mut c = fs::read_to_string(home.join(".zshrc")).unwrap();
    c.push_str("export ADDED_LATER=1\n");
    fs::write(home.join(".zshrc"), &c).unwrap();

    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));

    let mode_after_uninstall = fs::metadata(home.join(".zshrc")).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode_after_uninstall, 0o600, "uninstall's atomic_write_bytes must preserve mode");
}

/// The dry-run plan enumerates the manifest's touched files, autostart, and tree.
#[test]
fn plan_dry_run_reflects_manifest() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();

    let plan = plan_dry_run(home, &m);
    // A restore line for the .zshrc entry and a tree-delete line must appear.
    assert!(
        plan.iter().any(|l| l.contains(".zshrc")),
        "plan mentions the touched rc file: {plan:?}"
    );
    assert!(
        plan.iter().any(|l| l.contains(".hub")),
        "plan mentions deleting the hub tree: {plan:?}"
    );
}
