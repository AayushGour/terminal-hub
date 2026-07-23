//! Session record files: `~/.hub/sessions/<id>.json`. Written atomically
//! (temp file + rename) so a reader never observes a half-written record.
//! NEVER put pty bytes or env vars in a record — title/cmdline only.

use crate::paths::HubPaths;
use hub_proto::{Origin, SessionId, SessionInfo};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionRecord {
    pub record_version: u32,
    pub id: SessionId,
    pub origin: Origin,
    pub title: String,
    pub pid: u32,
    pub started_unix: u64,
    pub cols: u16,
    pub rows: u16,
    /// Absolute path to this relay's per-session socket.
    pub sock: String,
}

impl SessionRecord {
    pub fn to_info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id, origin: self.origin, title: self.title.clone(),
            pid: self.pid, started_unix: self.started_unix, cols: self.cols, rows: self.rows,
        }
    }

    /// Atomic write: serialize to `<id>.json.tmp` then rename over `<id>.json`.
    pub fn write_atomic(&self, paths: &HubPaths) -> anyhow::Result<()> {
        let final_path = paths.record(self.id);
        let tmp = final_path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &final_path)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<SessionRecord> {
        let bytes = std::fs::read(path)?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub fn delete(paths: &HubPaths, id: SessionId) {
        let _ = std::fs::remove_file(paths.record(id));
    }
}
