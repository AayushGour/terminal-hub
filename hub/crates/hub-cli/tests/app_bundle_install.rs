// `hub install --app-bundle <PATH>` / `hub uninstall` symmetry: install must
// place the `.app` bundle in /Applications and record it in the manifest;
// uninstall must remove exactly what was recorded. macOS-only (`ditto` +
// `/Applications`), matching `install::install_app_bundle`'s own gating.
#![cfg(target_os = "macos")]

use hub_cli::manifest;
use hub_cli::paths;
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// A unique, throwaway "app bundle" under /Applications so this test never
/// collides with a real install (or with itself running twice in parallel).
fn fake_bundle_name() -> String {
    format!("hub-test-{}.app", std::process::id())
}

fn setup() -> (tempfile::TempDir, tempfile::TempDir) {
    std::env::set_var("HUB_SKIP_SERVICE_ACTIVATION", "1");
    std::env::set_var("SHELL", "/bin/zsh");
    std::env::remove_var("HUB_DIR");
    std::env::remove_var("HUB_SOCK");

    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join(".zshrc"), "export FOO=bar\n").unwrap();

    let src = tempfile::tempdir().unwrap();
    for name in ["hub", "hub-daemon", "hub-relay"] {
        let p = src.path().join(name);
        fs::write(&p, format!("#!/bin/sh\necho fake-{name}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (home, src)
}

#[test]
fn install_places_app_bundle_and_uninstall_removes_it() {
    let (home_dir, src) = setup();
    let home = home_dir.path();

    // A minimal fake "app bundle" dir (ditto works on any directory, not just
    // a real signed .app) with one marker file inside, so we can prove ditto
    // actually copied real content rather than just creating an empty dir.
    let bundle_src = tempfile::tempdir().unwrap();
    let bundle_dir = bundle_src.path().join(fake_bundle_name());
    fs::create_dir_all(bundle_dir.join("Contents/MacOS")).unwrap();
    fs::write(bundle_dir.join("Contents/MacOS/marker"), b"hub-test-marker").unwrap();

    let dest = format!("/Applications/{}", fake_bundle_name());
    // Guard against ever touching a pre-existing real path (paranoia; the pid
    // in the name should already guarantee uniqueness).
    assert!(!std::path::Path::new(&dest).exists(), "destination must not pre-exist");

    hub_cli::install::run(home, true, Some(src.path()), Some(&bundle_dir)).unwrap();

    // 1. Bundle landed in /Applications with its content intact.
    let marker = fs::read_to_string(format!("{dest}/Contents/MacOS/marker")).unwrap();
    assert_eq!(marker, "hub-test-marker", "ditto must copy bundle contents verbatim");

    // 2. Manifest records the destination.
    let m = manifest::load(&paths::manifest_path(home)).unwrap();
    assert_eq!(m.app_bundle.as_deref(), Some(dest.as_str()));

    // 3. Uninstall removes exactly that path.
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(hub_cli::uninstall::run(home, true, false)).unwrap();
    assert!(
        !std::path::Path::new(&dest).exists(),
        "uninstall must remove the app bundle hub installed"
    );
}
