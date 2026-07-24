use hub_proto::{Origin, SessionId};
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;

#[test]
fn record_writes_atomically_and_loads() {
    let dir = std::env::temp_dir().join(format!("rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir);
    paths.ensure_dirs().unwrap();

    let rec = SessionRecord {
        record_version: 1, id: SessionId(42), origin: Origin::Hub,
        title: "x".into(), pid: 12345, started_unix: 100, cols: 120, rows: 40,
        sock: paths.sock(SessionId(42)).to_string_lossy().into(),
        cwd: "/home/u/proj".into(), last_exit_code: Some(0), activity_seq: 2,
    };
    rec.write_atomic(&paths).unwrap();

    let path = paths.record(SessionId(42));
    assert!(path.exists());
    let loaded = SessionRecord::load(&path).unwrap();
    assert_eq!(loaded.id, SessionId(42));
    assert_eq!(loaded.pid, 12345);
    assert_eq!(loaded.cwd, "/home/u/proj");
    assert_eq!(loaded.last_exit_code, Some(0));
    assert_eq!(loaded.activity_seq, 2);
    let info = loaded.to_info();
    assert_eq!(info.cols, 120);
    assert_eq!(info.cwd, "/home/u/proj");
    assert_eq!(info.last_exit_code, Some(0));
    assert_eq!(info.activity_seq, 2);

    SessionRecord::delete(&paths, SessionId(42));
    assert!(!path.exists());
}

// Backward compat: a record written by a relay from BEFORE this design
// (missing the cwd/last_exit_code/activity_seq fields entirely) must still
// load -- `#[serde(default)]` on the new fields, not a hard schema break.
#[test]
fn record_without_shell_integration_fields_still_loads() {
    let dir = std::env::temp_dir().join(format!("rec-oldshape-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let paths = HubPaths::new(dir);
    paths.ensure_dirs().unwrap();

    let old_json = serde_json::json!({
        "record_version": 1,
        "id": SessionId(7),
        "origin": "Hub",
        "title": "old",
        "pid": 99,
        "started_unix": 1,
        "cols": 80,
        "rows": 24,
        "sock": paths.sock(SessionId(7)).to_string_lossy(),
    });
    std::fs::write(paths.record(SessionId(7)), serde_json::to_vec(&old_json).unwrap()).unwrap();

    let loaded = SessionRecord::load(&paths.record(SessionId(7))).unwrap();
    assert_eq!(loaded.cwd, "");
    assert_eq!(loaded.last_exit_code, None);
    assert_eq!(loaded.activity_seq, 0);
}
