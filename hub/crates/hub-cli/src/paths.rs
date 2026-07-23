// Path-helper API surface for the CLI; several functions here are wired up
// by later Plan-3 tasks (install/uninstall/status/kill), so allow the
// not-yet-called ones to avoid dead_code warnings in the interim.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

// Task 8 carry-forward fix (Task 1 review): `hub_dir` used to hard-code
// `home.join(".hub")` and ignore `$HUB_DIR`, while the daemon/relay side
// (`hub_relay::paths::HubPaths::from_env`) resolves its base dir as `$HUB_DIR`
// if set, else `$HOME/.hub`. Test harnesses (hub-e2e, hub-daemon integration
// tests) start the daemon with `HUB_DIR=<tmp>` for isolation; if hub-cli kept
// computing `home/.hub` regardless, `hub status`/`hub kill`/`hub attach`
// would dial a socket the daemon never bound, and `hub status` would scan an
// empty (or wrong) sessions dir. `hub_dir` now honors `$HUB_DIR` the same way
// `HubPaths::from_env` does, so every path derived from it (sessions dir,
// logs dir, manifest, daemon socket) automatically agrees with the daemon's
// view of the world under the same environment.
pub fn hub_dir(home: &Path) -> PathBuf {
    if let Some(d) = std::env::var_os("HUB_DIR") {
        return PathBuf::from(d);
    }
    home.join(".hub")
}
pub fn sessions_dir(home: &Path) -> PathBuf { hub_dir(home).join("sessions") }
pub fn logs_dir(home: &Path) -> PathBuf { hub_dir(home).join("logs") }
pub fn backups_dir(home: &Path) -> PathBuf { hub_dir(home).join("backups") }
/// Self-contained binary drop (`hub`, `hub-daemon`, `hub-relay`) for
/// `hub install --bin-src`. HUB_DIR-aware like every other derived path, so
/// tests/harnesses that relocate the base dir keep it consistent.
pub fn bin_dir(home: &Path) -> PathBuf { hub_dir(home).join("bin") }
pub fn manifest_path(home: &Path) -> PathBuf { hub_dir(home).join("install-manifest.json") }

pub fn daemon_sock_path(home: &Path) -> PathBuf {
    // `HUB_SOCK` is a full-path override (highest precedence, for pointing at
    // an already-known socket without needing the base dir at all).
    if let Some(p) = std::env::var_os("HUB_SOCK") {
        return PathBuf::from(p);
    }
    // Otherwise derive from the (HUB_DIR-aware) base dir, matching
    // `hub_relay::paths::HubPaths::daemon_sock` exactly: `<base>/hubd.sock`.
    hub_dir(home).join("hubd.sock")
}

pub fn locate_sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cand = exe.parent()?.join(name);
    cand.exists().then_some(cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Mutex;

    // `HUB_DIR`/`HUB_SOCK` are process-global; `cargo test` runs `#[test]`
    // fns in this file in parallel threads within the same binary by
    // default, so mutating them without serialization would be racy between
    // e.g. `tree_paths_are_under_dot_hub` (expects both unset) and
    // `hub_dir_and_derived_paths_honor_hub_dir_env_override` (sets HUB_DIR).
    // Every test below that touches these env vars holds this lock for its
    // full body.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn tree_paths_are_under_dot_hub() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("HUB_DIR");
        let home = Path::new("/tmp/fakehome");
        assert_eq!(hub_dir(home), Path::new("/tmp/fakehome/.hub"));
        assert_eq!(sessions_dir(home), Path::new("/tmp/fakehome/.hub/sessions"));
        assert_eq!(manifest_path(home), Path::new("/tmp/fakehome/.hub/install-manifest.json"));
    }

    #[test]
    fn daemon_sock_defaults_under_dot_hub() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("HUB_SOCK");
        std::env::remove_var("HUB_DIR");
        let home = Path::new("/tmp/fakehome");
        assert_eq!(daemon_sock_path(home), Path::new("/tmp/fakehome/.hub/hubd.sock"));
    }

    /// Task 8 carry-forward: `$HUB_DIR` (the same env var `hub_relay::paths`
    /// honors) must override the base dir for the daemon socket AND the
    /// sessions dir, so `hub status`/`hub kill` talk to — and scan the
    /// records of — a daemon started under `HUB_DIR=<tmp>` by test harnesses,
    /// exactly as `hub_relay::paths::HubPaths::from_env` would resolve it.
    #[test]
    fn hub_dir_and_derived_paths_honor_hub_dir_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("HUB_SOCK");
        std::env::set_var("HUB_DIR", "/tmp/daemon-base");
        let home = Path::new("/tmp/fakehome"); // deliberately different from HUB_DIR
        assert_eq!(hub_dir(home), Path::new("/tmp/daemon-base"));
        assert_eq!(sessions_dir(home), Path::new("/tmp/daemon-base/sessions"));
        assert_eq!(daemon_sock_path(home), Path::new("/tmp/daemon-base/hubd.sock"));
        std::env::remove_var("HUB_DIR");
    }

    /// `HUB_SOCK` (a full-path override) must still win over `HUB_DIR`.
    #[test]
    fn hub_sock_env_overrides_hub_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("HUB_DIR", "/tmp/daemon-base");
        std::env::set_var("HUB_SOCK", "/tmp/explicit.sock");
        let home = Path::new("/tmp/fakehome");
        assert_eq!(daemon_sock_path(home), Path::new("/tmp/explicit.sock"));
        std::env::remove_var("HUB_SOCK");
        std::env::remove_var("HUB_DIR");
    }
}
