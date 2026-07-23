// The install manifest is the single source of truth for `hub uninstall`
// (Task 7): it records every file hub touched, the byte-for-byte backup taken
// before editing, the post-install hash (to spot later user edits), and any
// autostart entry. Fields not yet produced by this task are wired by later
// Plan-3 tasks; keep them stable so old manifests keep loading.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchedFile {
    pub path: String,
    /// Backup of the pre-install content; None if hub created the file.
    pub backup: Option<String>,
    /// True if hub created this file (uninstall deletes it entirely).
    pub created_by_hub: bool,
    /// sha256 of the file as install left it (used to detect later user edits).
    pub post_install_sha256: String,
    /// "snippet" | "bridge" — which marked block hub added.
    pub block: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutostartEntry {
    Launchd { plist: String, label: String },
    Systemd { unit: String, name: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<TouchedFile>,
    #[serde(default)]
    pub autostart: Option<AutostartEntry>,
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub install_prefix: Option<String>,
}

fn one() -> u32 {
    1
}

pub fn load(path: &Path) -> anyhow::Result<Manifest> {
    if !path.exists() {
        return Ok(Manifest {
            version: 1,
            ..Default::default()
        });
    }
    let s = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&s)?)
}

pub fn save(path: &Path, m: &Manifest) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(m)?)?;
    Ok(())
}
