//! Runtime filesystem layout. Honors `HUB_DIR` for test isolation; defaults
//! to `~/.hub`. Dir perms 0700 (enforced on ensure_dirs).

use hub_proto::SessionId;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct HubPaths {
    base: PathBuf,
}

impl HubPaths {
    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    /// `$HUB_DIR` if set, else `$HOME/.hub`.
    pub fn from_env() -> Self {
        if let Ok(d) = std::env::var("HUB_DIR") {
            return Self::new(PathBuf::from(d));
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        Self::new(Path::new(&home).join(".hub"))
    }

    pub fn base(&self) -> &Path { &self.base }
    pub fn daemon_sock(&self) -> PathBuf { self.base.join("hubd.sock") }
    pub fn sessions_dir(&self) -> PathBuf { self.base.join("sessions") }
    pub fn logs_dir(&self) -> PathBuf { self.base.join("logs") }
    pub fn record(&self, id: SessionId) -> PathBuf {
        self.sessions_dir().join(format!("{}.json", id.0))
    }
    pub fn sock(&self, id: SessionId) -> PathBuf {
        self.sessions_dir().join(format!("{}.sock", id.0))
    }

    /// Create base + sessions + logs dirs, chmod every one of them 0700 (the
    /// base dir alone is not enough: sessions/ holds per-session sockets and
    /// must itself resist a shared-user `ls`/traversal even if the base dir's
    /// mode were ever loosened).
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::create_dir_all(self.sessions_dir())?;
        std::fs::create_dir_all(self.logs_dir())?;
        for dir in [&self.base, &self.sessions_dir(), &self.logs_dir()] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
}
