//! Local-IPC authentication (F1) shared by every hub component.
//!
//! Two independent gates protect every hub socket (the daemon's `hubd.sock`
//! AND each relay's per-session `<id>.sock`):
//!
//! 1. **Per-install secret token** at `<HUB_DIR>/token` (0600). Any process
//!    that can READ this file is authorized — the token proves "I can read
//!    your ~/.hub", i.e. same-uid and not sandboxed away from it. A client
//!    sends it in a `ControlMsg::Hello { token }` as its FIRST frame; the
//!    server compares it against its own copy. The token is NEVER logged.
//! 2. **Peer-credential check** (defense in depth): on every accepted
//!    connection the server checks the connecting peer's uid == our uid via
//!    `getpeereid` (macOS/BSD) / `SO_PEERCRED` (Linux). A mismatch closes the
//!    connection before any frame is processed.
//!
//! This module owns the reusable primitives (`ensure_token`, `load_token`,
//! `send_hello`, `peer_uid_ok`); the frame-level `Hello` verification lives on
//! the readers (`hub_relay::conn::FrameReader::verify_hello` and
//! `FramedConn`), which reuse their own frame decoder so bytes pipelined after
//! `Hello` are never lost.

use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::RawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hub_proto::{encode_control, ControlMsg};
use tokio::io::{AsyncWrite, AsyncWriteExt};

/// Bound on how long a server waits for the mandatory `Hello` frame before
/// giving up. A silent/partial client must never be able to hang the accept
/// path — auth failure is always a prompt reject-and-close, never a hang.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The token file lives directly under the hub base dir.
pub fn token_path(base: &Path) -> PathBuf {
    base.join("token")
}

/// Read the per-install token from `<base>/token`. Errors if it does not exist
/// (a client that cannot read the token is, by design, not authorized).
pub fn load_token(base: &Path) -> anyhow::Result<String> {
    let path = token_path(base);
    let s = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("cannot read hub token at {}: {e}", path.display()))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        anyhow::bail!("hub token at {} is empty", path.display());
    }
    Ok(s)
}

/// Ensure a token exists at `<base>/token` (0600) and return it. If absent, it
/// is created with 32 bytes of OS randomness, hex-encoded. Idempotent and
/// race-safe: creation uses `O_CREAT | O_EXCL`, so if two processes race, the
/// loser observes `AlreadyExists` and reads the winner's token — both end up
/// with the same value. Called by the daemon on startup (lazy creation so
/// tests/e2e without a full `hub install` still work), by `hub install`, and
/// defensively by clients (reads the existing token in any live-daemon case).
pub fn ensure_token(base: &Path) -> anyhow::Result<String> {
    // Fast path: already present.
    if let Ok(tok) = load_token(base) {
        return Ok(tok);
    }
    std::fs::create_dir_all(base)
        .map_err(|e| anyhow::anyhow!("cannot create hub dir {}: {e}", base.display()))?;
    let token = gen_token()?;
    let path = token_path(base);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // O_CREAT | O_EXCL
        .mode(0o600)
        .open(&path)
    {
        Ok(mut f) => {
            f.write_all(token.as_bytes())?;
            f.flush()?;
            // `mode()` above is subject to umask; force 0600 explicitly so the
            // token is never group/other-readable regardless of umask.
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(token)
        }
        // Lost the create race: another process wrote it first — read theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_token(base),
        Err(e) => Err(anyhow::anyhow!("cannot create hub token {}: {e}", path.display())),
    }
}

/// 32 bytes of OS randomness from `/dev/urandom`, hex-encoded (64 chars).
/// Uses the OS CSPRNG directly (no workflow-forbidden userspace RNG).
fn gen_token() -> anyhow::Result<String> {
    let mut buf = [0u8; 32];
    let mut f = std::fs::File::open("/dev/urandom")
        .map_err(|e| anyhow::anyhow!("open /dev/urandom: {e}"))?;
    f.read_exact(&mut buf)
        .map_err(|e| anyhow::anyhow!("read /dev/urandom: {e}"))?;
    let mut out = String::with_capacity(64);
    for b in buf {
        out.push_str(&format!("{b:02x}"));
    }
    Ok(out)
}

/// Pre-encoded `Hello { token }` frame (for use with pre-encoded writers like
/// `FramedConn::write_frame`).
pub fn hello_frame(token: &str) -> Vec<u8> {
    encode_control(&ControlMsg::Hello { token: token.to_string() })
}

/// Write the mandatory `Hello { token }` as the first frame on `wr` and flush.
/// Every client MUST call this before any session op.
pub async fn send_hello<W: AsyncWrite + Unpin>(wr: &mut W, token: &str) -> anyhow::Result<()> {
    wr.write_all(&hello_frame(token)).await?;
    wr.flush().await?;
    Ok(())
}

/// True iff the connected peer's uid equals our own uid.
///
/// macOS/BSD: `getpeereid`. Linux: `SO_PEERCRED`. This is defense-in-depth on
/// top of the 0600 token: even a same-uid caller must present the token, and a
/// different-uid caller (which cannot read the token anyway) is refused here
/// regardless. `fd` must be a connected `AF_UNIX` stream socket.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd"))]
pub fn peer_uid_ok(fd: RawFd) -> bool {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: `fd` is a valid connected unix-domain socket for the lifetime of
    // this call; getpeereid writes the peer's effective uid/gid into the outs.
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    rc == 0 && uid == unsafe { libc::getuid() }
}

#[cfg(target_os = "linux")]
pub fn peer_uid_ok(fd: RawFd) -> bool {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED writes a `struct ucred` for a connected AF_UNIX
    // socket; `len` is initialized to its size and updated in place.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    rc == 0 && cred.uid == unsafe { libc::getuid() }
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "linux"
)))]
pub fn peer_uid_ok(_fd: RawFd) -> bool {
    // Unknown platform: no reliable peer-cred API. Fail closed.
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn ensure_token_creates_0600_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let t1 = ensure_token(base).unwrap();
        assert_eq!(t1.len(), 64, "32 bytes hex-encoded");
        assert!(t1.chars().all(|c| c.is_ascii_hexdigit()));

        let mode = std::fs::metadata(token_path(base)).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "token file must be 0600");

        // Idempotent: a second call returns the SAME token, not a fresh one.
        let t2 = ensure_token(base).unwrap();
        assert_eq!(t1, t2);
        assert_eq!(load_token(base).unwrap(), t1);
    }

    #[test]
    fn load_token_errors_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_token(dir.path()).is_err());
    }

    // Peer-cred: cross-uid is not testable in a normal (single-uid) test env —
    // that case is verified manually. Here we assert the SAME-uid path: both
    // ends of a locally-connected socket share our uid, so `peer_uid_ok` must
    // return true. This exercises the real getpeereid/SO_PEERCRED code path.
    #[tokio::test]
    async fn peer_uid_ok_true_for_same_uid_peer() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("t.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let accept = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            peer_uid_ok(s.as_raw_fd())
        });
        let client = tokio::net::UnixStream::connect(&sock).await.unwrap();
        assert!(peer_uid_ok(client.as_raw_fd()), "client side sees same-uid server");
        assert!(accept.await.unwrap(), "server side sees same-uid client");
    }
}
