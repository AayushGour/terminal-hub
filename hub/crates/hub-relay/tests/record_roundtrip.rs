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
    };
    rec.write_atomic(&paths).unwrap();

    let path = paths.record(SessionId(42));
    assert!(path.exists());
    let loaded = SessionRecord::load(&path).unwrap();
    assert_eq!(loaded.id, SessionId(42));
    assert_eq!(loaded.pid, 12345);
    assert_eq!(loaded.to_info().cols, 120);

    SessionRecord::delete(&paths, SessionId(42));
    assert!(!path.exists());
}
