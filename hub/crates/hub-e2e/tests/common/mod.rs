#![allow(dead_code)]
use hub_relay::paths::HubPaths;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub fn workspace_bin(name: &str) -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.push(format!("../../target/debug/{name}"));
    p
}

pub fn build_all() {
    let ok = Command::new("cargo")
        .args(["build", "-p", "hub-daemon", "-p", "hub-relay", "-p", "hub-tui"])
        .status().unwrap().success();
    assert!(ok, "build all bins");
}

pub fn alive(pid: u32) -> bool { unsafe { libc::kill(pid as i32, 0) == 0 } }

/// `run_relay` binds the per-session socket BEFORE it writes the record file,
/// so a test that waits on the socket and then immediately loads the record
/// can race the writer by a sub-millisecond window. Poll instead of a bare
/// `.unwrap()` on the first attempt.
pub async fn load_record_retry(p: &std::path::Path) -> hub_relay::record::SessionRecord {
    for _ in 0..50 {
        if let Ok(r) = hub_relay::record::SessionRecord::load(p) {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    hub_relay::record::SessionRecord::load(p).expect("record never appeared")
}

pub struct Harness {
    pub dir: PathBuf,
    pub paths: HubPaths,
    pub daemon: Option<Child>,
}

impl Harness {
    pub async fn start() -> Harness {
        build_all();
        let dir = std::env::temp_dir().join(format!("e2e-{}-{}", std::process::id(), rand_suffix()));
        let _ = std::fs::remove_dir_all(&dir);
        let paths = HubPaths::new(dir.clone());
        paths.ensure_dirs().unwrap();
        let mut h = Harness { dir, paths, daemon: None };
        h.start_daemon().await;
        h
    }

    async fn start_daemon(&mut self) {
        let child = Command::new(workspace_bin("hub-daemon"))
            .env("HUB_DIR", &self.dir)
            .stdout(Stdio::null()).stderr(Stdio::null())
            .spawn().unwrap();
        self.daemon = Some(child);
        self.wait_path(&self.paths.daemon_sock()).await;
    }

    pub fn kill_daemon(&mut self) {
        if let Some(mut d) = self.daemon.take() { let _ = d.kill(); let _ = d.wait(); }
    }

    pub async fn restart_daemon(&mut self) {
        self.kill_daemon();
        // Daemon binds a fresh listener; reconciliation re-adopts live relays.
        let _ = std::fs::remove_file(self.paths.daemon_sock());
        self.start_daemon().await;
    }

    /// External relay: we hold its outer-terminal stdin pipe. Returns the Child
    /// so the test controls the outer terminal lifetime.
    pub fn spawn_external_relay(&self, shell: &str) -> Child {
        Command::new(workspace_bin("hub-relay"))
            .args(["--origin","external","--shell",shell,"--size","80x24",
                   "--daemon-sock", self.paths.daemon_sock().to_str().unwrap()])
            .env("HUB_DIR", &self.dir)
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().unwrap()
    }

    /// Hub relay: detached (double-fork). Fire-and-forget; find its pid via the
    /// record file.
    pub fn spawn_hub_relay(&self, shell: &str) {
        Command::new(workspace_bin("hub-relay"))
            .args(["--detach","--origin","hub","--shell",shell,"--size","80x24",
                   "--daemon-sock", self.paths.daemon_sock().to_str().unwrap()])
            .env("HUB_DIR", &self.dir)
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .status().unwrap();
    }

    pub async fn wait_path(&self, p: &std::path::Path) {
        for _ in 0..400 { if p.exists() { return; } tokio::time::sleep(Duration::from_millis(10)).await; }
        panic!("timed out waiting for {p:?}");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.kill_daemon();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().subsec_nanos() as u64
}
