use hub_proto::{SessionId, SessionInfo};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

// NOTE: on-disk records (`hub_relay::record::SessionRecord`) also carry a
// `record_version` field. It is intentionally NOT modeled here (matches the
// brief's authoritative Interfaces section: `RecordFile { info, sock }`) —
// serde ignores unknown JSON fields by default (no `deny_unknown_fields`),
// so `record_version` is tolerated-but-dropped on deserialize for free.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordFile {
    #[serde(flatten)]
    pub info: SessionInfo,
    pub sock: String,
}

pub fn scan_records(dir: &Path) -> Vec<RecordFile> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(rec) = serde_json::from_str::<RecordFile>(&s) {
                    out.push(rec);
                }
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct Buckets {
    pub healthy: Vec<SessionInfo>,
    pub ghost: Vec<RecordFile>,
    pub orphan: Vec<SessionInfo>,
}

pub fn reconcile(
    live: &[SessionInfo],
    records: &[RecordFile],
    sock_alive: &dyn Fn(&Path) -> bool,
) -> Buckets {
    let live_ids: HashSet<SessionId> = live.iter().map(|s| s.id).collect();
    let record_ids: HashSet<SessionId> = records.iter().map(|r| r.info.id).collect();

    let mut b = Buckets::default();
    for s in live {
        if record_ids.contains(&s.id) {
            b.healthy.push(s.clone());
        } else {
            b.orphan.push(s.clone()); // live but no record
        }
    }
    for r in records {
        if !live_ids.contains(&r.info.id) && !sock_alive(Path::new(&r.sock)) {
            b.ghost.push(r.clone()); // recorded, daemon doesn't see it, socket dead
        }
    }
    b
}
