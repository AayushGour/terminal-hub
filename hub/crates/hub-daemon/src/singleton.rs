//! Daemon singleton guard (contract §J).
//!
//! `hub_transport::bind_listener` unconditionally unlinks a stale `hubd.sock`
//! before binding, so a second daemon started under the same `HUB_DIR` would
//! otherwise silently steal the socket out from under a live one. This
//! module makes that safe: only a process holding an exclusive, non-blocking
//! `flock` on `<HUB_DIR>/hubd.lock` is allowed to reach `bind_listener` at
//! all, so at most one daemon per `HUB_DIR` ever binds `hubd.sock`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::Path;

use nix::fcntl::{flock, FlockArg};

/// Holds the exclusive lock for as long as it's alive. The `flock` is
/// released automatically by the kernel when this (and thus the underlying
/// fd) is dropped, i.e. when the daemon process exits or explicitly drops
/// the guard.
pub struct SingletonGuard {
    _file: File,
}

/// Acquire the daemon singleton lock at `lock_path` (conventionally
/// `<HUB_DIR>/hubd.lock`).
///
/// Returns `Err` if another live process already holds the lock — in that
/// case the caller MUST NOT proceed to `bind_listener`/unlink `hubd.sock`,
/// since a live daemon is still serving it.
pub fn acquire(lock_path: &Path) -> anyhow::Result<SingletonGuard> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(lock_path)
        .map_err(|e| anyhow::anyhow!("failed to open lockfile {:?}: {e}", lock_path))?;

    flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock).map_err(|e| {
        anyhow::anyhow!(
            "daemon already running (could not acquire exclusive lock on {:?}: {e})",
            lock_path
        )
    })?;

    // Best-effort diagnostics: record our pid in the lockfile. Not load-
    // bearing for correctness -- the flock itself is the source of truth.
    let _ = file.set_len(0);
    let _ = write!(file, "{}", std::process::id());
    let _ = file.flush();

    Ok(SingletonGuard { _file: file })
}
