// Task 4: reconciliation buckets (healthy / ghost / orphan).
//
// The daemon protocol has no "give me the buckets" wire message -- only
// `ControlMsg::List` -> `Sessions { sessions }` (the live set, from
// `ConnManager::list_sessions` over a short-lived connection). The disk-side truth is the set of
// `~/.hub/sessions/<id>.json` record files each relay writes (see
// `hub_relay::record::SessionRecord` / spec §C). This module diffs the two:
//
// - healthy = live AND recorded
// - ghost   = recorded but NOT live (the daemon doesn't know it -- relay
//             likely crashed; disk record is stale)
// - orphan  = live but NOT recorded (daemon knows it, no record file --
//             e.g. record write raced/failed)
//
// `bucketize` is a pure function over two already-loaded `Vec`s so it's
// unit-testable without a filesystem or a daemon connection (see `mod
// tests` below). `reconcile_sessions` is the thin Tauri-command wrapper that
// gathers the two inputs (`DaemonClient::list` + `read_records`) and calls it.

use hub_proto::{Origin, SessionInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// On-disk `~/.hub/sessions/<id>.json` shape written by
/// `hub_relay::record::SessionRecord`: `SessionInfo` fields flattened plus
/// `sock` (the relay's per-session socket path) and `record_version`. Field
/// *names* must match `SessionRecord` exactly for `serde_json` to populate
/// them (field order doesn't matter for a struct, only for a tuple/seq) --
/// this type is intentionally a structural duplicate rather than a
/// dependency on `hub-relay` (which pulls in `hub-pty`/`hub-term`/pty deps
/// this GUI backend has no other use for; same rationale as `daemon.rs`'s
/// private `hub_base_dir`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GhostRecord {
    pub id: u64,
    pub origin: Origin,
    pub title: String,
    pub pid: u32,
    pub started_unix: u64,
    pub cols: u16,
    pub rows: u16,
    // Spec §5 (2026-07-23-shell-integration-design.md): mirrors the 3 new
    // `SessionInfo`/`SessionRecord` fields so a ghost record still shows the
    // last-known cwd/exit code after a crashed relay's final disk write.
    // `#[serde(default)]` matches `hub_relay::record::SessionRecord`'s own
    // choice: an on-disk record written before this feature existed lacks
    // these keys entirely, and per this file's own doc comment on
    // `read_records`, "one corrupt record must not hide every other session"
    // -- without `default`, every pre-existing ghost record on disk would
    // fail to deserialize and silently vanish from the ghost bucket instead
    // of just showing a blank cwd/no exit code.
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub last_exit_code: Option<i32>,
    #[serde(default)]
    pub activity_seq: u64,
    pub sock: String,
    pub record_version: u32,
}

#[derive(Serialize)]
pub struct Buckets {
    pub healthy: Vec<SessionInfo>,
    pub ghost: Vec<GhostRecord>,
    pub orphan: Vec<SessionInfo>,
}

/// Pure diff of the daemon's live set against the on-disk records. No IO.
pub fn bucketize(live: Vec<SessionInfo>, records: Vec<GhostRecord>) -> Buckets {
    let live_ids: HashSet<u64> = live.iter().map(|s| s.id.0).collect();
    let record_ids: HashSet<u64> = records.iter().map(|r| r.id).collect();

    let healthy = live.iter().filter(|s| record_ids.contains(&s.id.0)).cloned().collect();
    let orphan = live.iter().filter(|s| !record_ids.contains(&s.id.0)).cloned().collect();
    let ghost = records.into_iter().filter(|r| !live_ids.contains(&r.id)).collect();
    Buckets { healthy, ghost, orphan }
}

/// Read every `<id>.json` record file in `sessions_dir`. Best-effort: a
/// missing dir yields an empty `Vec` (daemon/relay may not have run yet), and
/// an unreadable/malformed individual file is skipped rather than failing
/// the whole scan -- one corrupt record must not hide every other session
/// from the GUI.
fn read_records(sessions_dir: &std::path::Path) -> Vec<GhostRecord> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions_dir) else { return out; };
    for e in entries.flatten() {
        if e.path().extension().and_then(|x| x.to_str()) != Some("json") { continue; }
        if let Ok(txt) = std::fs::read_to_string(e.path()) {
            if let Ok(rec) = serde_json::from_str::<GhostRecord>(&txt) {
                out.push(rec);
            }
        }
    }
    out
}

#[tauri::command]
pub async fn reconcile_sessions(
    state: tauri::State<'_, crate::commands::AppState>,
) -> Result<Buckets, String> {
    let mgr = crate::commands::manager(&state)?;
    // Short-lived List connection (never a persistent viewer conn).
    let live = mgr.list_sessions().await.map_err(|e| e.to_string())?;
    let dir = crate::hub_home_sessions(); // HUB_DIR-aware (see lib.rs::hub_home)
    let records = read_records(&dir);
    Ok(bucketize(live, records))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{Origin, SessionId, SessionInfo};

    fn info(id: u64) -> SessionInfo {
        SessionInfo { id: SessionId(id), origin: Origin::External, title: format!("s{id}"),
                      pid: 1, started_unix: 1, cols: 80, rows: 24,
                      cwd: String::new(), last_exit_code: None, activity_seq: 0 }
    }
    fn rec(id: u64) -> GhostRecord {
        GhostRecord { id, origin: Origin::External, title: format!("s{id}"), pid: 1,
                      started_unix: 1, cols: 80, rows: 24,
                      cwd: String::new(), last_exit_code: None, activity_seq: 0,
                      sock: format!("/tmp/{id}.sock"), record_version: 1 }
    }

    #[test]
    fn healthy_ghost_orphan_split() {
        // live: 1,2 ; recorded: 2,3
        let b = bucketize(vec![info(1), info(2)], vec![rec(2), rec(3)]);
        assert_eq!(b.healthy.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![2]); // live ∩ recorded
        assert_eq!(b.ghost.iter().map(|g| g.id).collect::<Vec<_>>(), vec![3]);     // recorded − live
        assert_eq!(b.orphan.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![1]);  // live − recorded
    }

    #[test]
    fn all_live_recorded_is_all_healthy() {
        let b = bucketize(vec![info(1), info(2)], vec![rec(1), rec(2)]);
        assert_eq!(b.healthy.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![1, 2]);
        assert!(b.ghost.is_empty());
        assert!(b.orphan.is_empty());
    }

    #[test]
    fn read_records_missing_dir_is_empty_not_error() {
        let dir = std::path::Path::new("/nonexistent/hub-reconcile-test-dir");
        assert!(read_records(dir).is_empty());
    }

    #[test]
    fn read_records_round_trips_written_files_and_skips_junk() {
        let tmp = tempfile::tempdir().unwrap();
        let good = rec(9);
        std::fs::write(tmp.path().join("9.json"), serde_json::to_vec(&good).unwrap()).unwrap();
        // Malformed record: must be skipped, not panic the whole scan.
        std::fs::write(tmp.path().join("10.json"), b"not json").unwrap();
        // Non-.json file: must be ignored.
        std::fs::write(tmp.path().join("notes.txt"), b"hi").unwrap();

        let recs = read_records(tmp.path());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].id, 9);
        assert_eq!(recs[0].sock, "/tmp/9.sock");
    }

    // A record file written by a relay from BEFORE the shell-integration
    // fields existed has no `cwd`/`last_exit_code`/`activity_seq` keys at
    // all. `#[serde(default)]` on those 3 `GhostRecord` fields must let it
    // still deserialize (cwd empty, last_exit_code None, activity_seq 0)
    // instead of being silently dropped by `read_records`' skip-on-error path.
    #[test]
    fn read_records_defaults_shell_integration_fields_for_pre_existing_record() {
        let tmp = tempfile::tempdir().unwrap();
        let old_format = serde_json::json!({
            "id": 7, "origin": "External", "title": "s7", "pid": 1,
            "started_unix": 1, "cols": 80, "rows": 24,
            "sock": "/tmp/7.sock", "record_version": 1,
        });
        std::fs::write(tmp.path().join("7.json"), serde_json::to_vec(&old_format).unwrap()).unwrap();

        let recs = read_records(tmp.path());
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].cwd, "");
        assert_eq!(recs[0].last_exit_code, None);
        assert_eq!(recs[0].activity_seq, 0);
    }
}
