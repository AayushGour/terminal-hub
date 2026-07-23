use hub_cli::autostart::{launchd_plist, systemd_unit, LAUNCHD_LABEL, SYSTEMD_NAME};
use std::path::Path;

#[test]
fn launchd_plist_has_label_program_and_runatload() {
    let p = launchd_plist(Path::new("/opt/hub/hub-daemon"));
    assert!(p.contains(LAUNCHD_LABEL));
    assert!(p.contains("<string>/opt/hub/hub-daemon</string>"));
    assert!(p.contains("<key>RunAtLoad</key>"));
    assert!(p.contains("<key>KeepAlive</key>"));
    assert!(p.trim_start().starts_with("<?xml"));
}

#[test]
fn systemd_unit_has_execstart_restart_and_wantedby() {
    let u = systemd_unit(Path::new("/opt/hub/hub-daemon"));
    assert!(u.contains(r#"ExecStart="/opt/hub/hub-daemon""#));
    assert!(u.contains("Restart=on-failure"));
    assert!(u.contains("WantedBy=default.target"));
    assert_eq!(SYSTEMD_NAME, "hub-daemon.service");
}

#[test]
fn paths_with_spaces_and_specials_are_escaped() {
    let p = launchd_plist(Path::new("/Users/a b/Library/App & Stuff/hub-daemon"));
    // The raw ampersand must not appear unescaped; it must be encoded as an entity.
    assert!(!p.contains(" & "));
    assert!(p.contains("&amp;"));
    // Spaces are valid inside XML text content and must survive verbatim.
    assert!(p.contains("<string>/Users/a b/Library/App &amp; Stuff/hub-daemon</string>"));

    let u = systemd_unit(Path::new("/home/u/my apps/hub-daemon"));
    // The path must be wrapped in double quotes so systemd doesn't word-split
    // on the embedded space.
    assert!(u.contains(r#"ExecStart="/home/u/my apps/hub-daemon""#));
}
