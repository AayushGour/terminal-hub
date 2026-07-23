// Task 10: scrollback (buffer-size) preference, persisted to
// `<hub_home>/config.json`. Per the interface contract, this only ever
// governs xterm's client-side scrollback for *newly opened* tiles -- it
// cannot retroactively resize a running relay's vt buffer, whose 10k ring is
// fixed at spawn (spec §10). `get_buffer_size`/`set_buffer_size` are the
// Tauri commands the frontend (`api.ts`, Task 5) already declared; the pure
// `load_scrollback`/`store_scrollback` helpers are unit-tested directly
// against a tempdir so the persistence logic doesn't need a real `$HOME` or
// a Tauri runtime to verify.
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_SCROLLBACK: u32 = 10_000;

/// Sane bounds for the persisted scrollback preference: below `MIN_SCROLLBACK`
/// the buffer is nearly useless (and xterm gets pathological with 0), above
/// `MAX_SCROLLBACK` a single tile's client-side buffer risks runaway memory
/// (spec §10's per-tile RAM tradeoff, multiplied across every open tile).
pub const MIN_SCROLLBACK: u32 = 100;
pub const MAX_SCROLLBACK: u32 = 1_000_000;

fn default_scrollback() -> u32 {
    DEFAULT_SCROLLBACK
}

#[derive(Serialize, Deserialize, Clone)]
struct Config {
    #[serde(default = "default_scrollback")]
    scrollback: u32,
    /// First-run consent: `true` once the user has clicked "Not now" on the
    /// setup dialog, so the GUI stops prompting on every launch (Settings can
    /// still offer Install later). `#[serde(default)]` so older single-field
    /// config files keep parsing (declined defaults to false).
    #[serde(default)]
    declined: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            scrollback: DEFAULT_SCROLLBACK,
            declined: false,
        }
    }
}

fn config_path(dir: &Path) -> std::path::PathBuf {
    dir.join("config.json")
}

/// Load the full persisted `Config` from `<dir>/config.json`, falling back to
/// `Config::default()` if the file is missing, unreadable, or holds JSON that
/// doesn't parse (corrupt config must never crash startup). Load-modify-save is
/// funneled through this so writing one field (scrollback OR declined) never
/// clobbers the other.
fn load_config(dir: &Path) -> Config {
    match std::fs::read_to_string(config_path(dir)) {
        Ok(txt) => serde_json::from_str::<Config>(&txt).unwrap_or_default(),
        Err(_) => Config::default(),
    }
}

fn save_config(dir: &Path, c: &Config) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(config_path(dir), serde_json::to_string_pretty(c)?)?;
    Ok(())
}

/// Load the persisted scrollback preference, falling back to
/// `DEFAULT_SCROLLBACK`.
pub fn load_scrollback(dir: &Path) -> u32 {
    load_config(dir).scrollback
}

/// Persist `size` as the scrollback preference under `<dir>/config.json`,
/// creating `dir` (e.g. `~/.hub`) if it doesn't exist yet. Preserves the
/// `declined` flag across the write.
pub fn store_scrollback(dir: &Path, size: u32) -> anyhow::Result<()> {
    let mut c = load_config(dir);
    c.scrollback = size;
    save_config(dir, &c)
}

/// Load the first-run "declined setup" flag (false if absent/corrupt).
pub fn load_declined(dir: &Path) -> bool {
    load_config(dir).declined
}

/// Persist the first-run "declined setup" flag, preserving `scrollback`.
pub fn store_declined(dir: &Path, declined: bool) -> anyhow::Result<()> {
    let mut c = load_config(dir);
    c.declined = declined;
    save_config(dir, &c)
}

/// FIX 2: clamp a requested scrollback value into `[MIN_SCROLLBACK,
/// MAX_SCROLLBACK]` before it's ever persisted, so `set_buffer_size(0)` or
/// `set_buffer_size(u32::MAX)` can't wedge `config.json` (and therefore every
/// newly opened xterm tile) with an unusable or memory-hostile value. Pure
/// and Tauri-free so it's directly unit-testable.
pub fn clamp_scrollback(size: u32) -> u32 {
    size.clamp(MIN_SCROLLBACK, MAX_SCROLLBACK)
}

#[tauri::command]
pub fn get_buffer_size() -> u32 {
    load_scrollback(&crate::hub_home())
}

#[tauri::command]
pub fn set_buffer_size(size: u32) -> Result<(), String> {
    store_scrollback(&crate::hub_home(), clamp_scrollback(size)).map_err(|e| e.to_string())
}

/// Has the user declined the first-run setup prompt? The frontend reads this on
/// startup so a prior "Not now" is honored across launches (no re-nag), while
/// Settings can still offer Install later.
#[tauri::command]
pub fn get_setup_declined() -> bool {
    load_declined(&crate::hub_home())
}

#[tauri::command]
pub fn set_setup_declined(declined: bool) -> Result<(), String> {
    store_declined(&crate::hub_home(), declined).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_10k_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_scrollback(dir.path()), 10_000);
    }

    #[test]
    fn store_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        store_scrollback(dir.path(), 50_000).unwrap();
        assert_eq!(load_scrollback(dir.path()), 50_000);
    }

    // FIX 2: `set_buffer_size` must clamp before persisting, so a garbage
    // value (0, u32::MAX) from the frontend can never land in config.json.
    // Exercised via the pure `clamp_scrollback` (no HUB_DIR/env dependency,
    // so it's race-free under parallel test execution) plus a
    // store_scrollback round-trip proving the clamped value is what
    // actually reaches disk.
    #[test]
    fn clamp_scrollback_raises_zero_up_to_minimum() {
        assert_eq!(clamp_scrollback(0), MIN_SCROLLBACK);
    }

    #[test]
    fn clamp_scrollback_caps_huge_value_at_maximum() {
        assert_eq!(clamp_scrollback(u32::MAX), MAX_SCROLLBACK);
    }

    #[test]
    fn clamp_scrollback_passes_through_in_range_values() {
        assert_eq!(clamp_scrollback(50_000), 50_000);
    }

    #[test]
    fn store_scrollback_with_clamped_zero_persists_at_least_minimum() {
        let dir = tempfile::tempdir().unwrap();
        store_scrollback(dir.path(), clamp_scrollback(0)).unwrap();
        assert!(load_scrollback(dir.path()) >= MIN_SCROLLBACK);
    }

    #[test]
    fn store_scrollback_with_clamped_huge_value_persists_at_most_maximum() {
        let dir = tempfile::tempdir().unwrap();
        store_scrollback(dir.path(), clamp_scrollback(u32::MAX)).unwrap();
        assert!(load_scrollback(dir.path()) <= MAX_SCROLLBACK);
    }

    #[test]
    fn declined_defaults_false_and_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!load_declined(dir.path()));
        store_declined(dir.path(), true).unwrap();
        assert!(load_declined(dir.path()));
    }

    // Writing one field must not clobber the other: the first-run consent flag
    // and the scrollback preference share `config.json`, so a load-modify-save
    // of either has to preserve the sibling.
    #[test]
    fn writing_declined_preserves_scrollback_and_vice_versa() {
        let dir = tempfile::tempdir().unwrap();
        store_scrollback(dir.path(), 42_000).unwrap();
        store_declined(dir.path(), true).unwrap();
        assert_eq!(load_scrollback(dir.path()), 42_000);
        assert!(load_declined(dir.path()));

        store_scrollback(dir.path(), 7_000).unwrap();
        assert!(load_declined(dir.path()), "scrollback write kept declined");
        assert_eq!(load_scrollback(dir.path()), 7_000);
    }

    // An older config.json with only `scrollback` must still parse, defaulting
    // declined to false (forward-compat).
    #[test]
    fn legacy_single_field_config_parses_with_declined_false() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"scrollback":33000}"#).unwrap();
        assert_eq!(load_scrollback(dir.path()), 33_000);
        assert!(!load_declined(dir.path()));
    }
}
