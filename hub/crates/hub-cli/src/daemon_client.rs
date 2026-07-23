// Real daemon-client RPCs (Plan 3, Task 8), consumed by `hub status`
// (Task 8), `hub kill` (Task 9), and `hub uninstall` (Task 7). Talks to the
// daemon's control socket over `hub-transport::FramedConn`.
use anyhow::anyhow;
use hub_proto::{ControlMsg, Frame, SessionId, SessionInfo};
use hub_transport::{connect, FramedConn};
use std::path::Path;
use std::time::Duration;

/// Bound on how long `kill_session` waits for the daemon/relay to ack a Kill
/// before treating the lack of a reply as best-effort success (see A4 below).
const KILL_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// F1 auth: every CLI connection to the daemon MUST send `Hello { token }` as
/// its FIRST frame before any op, or the daemon closes the connection without
/// processing anything. The per-install token lives at `<base>/token`, where
/// `base` is the daemon socket's parent dir (`<base>/hubd.sock`);
/// `ensure_token` reads the daemon-created token (and, for self-contained
/// fakes/tests dialing a socket in a temp dir, lazily creates a matching one).
/// The token is never logged.
async fn connect_hello(sock: &Path) -> anyhow::Result<FramedConn> {
    let base = sock
        .parent()
        .ok_or_else(|| anyhow!("daemon socket path has no parent dir for token lookup"))?;
    let token = hub_transport::auth::ensure_token(base)?;
    let mut conn: FramedConn = connect(sock).await?;
    conn.send_hello(&token).await?;
    Ok(conn)
}

async fn one_shot(sock: &Path, msg: ControlMsg) -> anyhow::Result<Frame> {
    let mut conn: FramedConn = connect_hello(sock).await?;
    conn.write_frame(&hub_proto::encode_control(&msg)).await?;
    conn.read_frame().await
}

/// List every session the daemon currently knows about (A3: List → Sessions).
pub async fn list_sessions(sock: &Path) -> anyhow::Result<Vec<SessionInfo>> {
    match one_shot(sock, ControlMsg::List).await? {
        Frame::Control(ControlMsg::Sessions { sessions }) => Ok(sessions),
        Frame::Control(ControlMsg::Error { message }) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected reply to List: {other:?}")),
    }
}

/// Kill a single session by id (used by Task 9's `hub kill`).
///
/// The real daemon (`hub-daemon/src/server.rs::handle_conn`) only accepts
/// `Open`/`List`/`Attach` as the FIRST frame on a connection; a bare `Kill`
/// as the first frame falls into the catch-all arm, which logs a warning and
/// drops the connection with no reply. `Kill` is only routed to the relay
/// from inside `drive_viewer_attached`, i.e. only once the connection has
/// registered as a viewer via `Attach{id}`. So the handshake here mirrors
/// what `hub-daemon/tests/teardown_origin.rs::hub_survives_detach_dies_on_kill`
/// drives against the real daemon: one connection, `Attach{id}` first, then
/// `Kill{id}` on the SAME connection.
///
/// After that, the daemon forwards Kill to the relay and (per A4) acks with
/// `Closed{id,..}` once the relay confirms; we also treat an `Error` reply,
/// a clean connection close (EOF), or a read timeout as terminal — a close
/// or timeout is treated as best-effort success since the relay process (and
/// therefore the daemon's session bookkeeping) may tear down/exit before it
/// gets a chance to ack.
pub async fn kill_session(sock: &Path, id: SessionId) -> anyhow::Result<()> {
    let mut conn: FramedConn = connect_hello(sock).await?;
    conn.write_frame(&hub_proto::encode_control(&ControlMsg::Attach { id }))
        .await?;
    conn.write_frame(&hub_proto::encode_control(&ControlMsg::Kill { id }))
        .await?;

    // Drain frames until we see a definitive Closed/Error, the connection
    // closes (EOF), or we time out waiting — any of the latter two are
    // treated as best-effort success per the A4 contract.
    loop {
        match tokio::time::timeout(KILL_ACK_TIMEOUT, conn.read_frame()).await {
            Ok(Ok(Frame::Control(ControlMsg::Closed { id: cid, .. }))) if cid == id => {
                return Ok(());
            }
            Ok(Ok(Frame::Control(ControlMsg::Error { message }))) => {
                return Err(anyhow!(message));
            }
            Ok(Ok(_other)) => {
                // Replay/Data/other viewer traffic from the attach; keep
                // draining until Closed/Error/EOF/timeout.
                continue;
            }
            Ok(Err(_)) => {
                // Connection closed by peer (EOF) — best-effort success.
                return Ok(());
            }
            Err(_) => {
                // Timed out waiting for a reply — best-effort success.
                return Ok(());
            }
        }
    }
}

/// Best-effort daemon shutdown: kill every live session, then stop the
/// daemon PROCESS itself via `ControlMsg::Shutdown` (autostart removal
/// remains a fallback in `hub uninstall` for the case where the daemon is
/// already unreachable). Never errors — `hub uninstall` must proceed even if
/// the daemon is already down.
///
/// Order matters: sessions are killed FIRST, while the daemon is still up to
/// route `Kill` to the relays; `Shutdown` is sent last since it stops the
/// daemon process (a `Shutdown`'d daemon can no longer route anything, but
/// the relays it leaves behind are untouched by design — see
/// `hub-daemon/src/server.rs`'s `Shutdown` handler).
pub async fn shutdown_daemon(sock: &Path) -> anyhow::Result<()> {
    if let Ok(sessions) = list_sessions(sock).await {
        for s in sessions {
            let _ = kill_session(sock, s.id).await;
        }
    }

    // Stop the daemon process. If connect fails, the daemon is already
    // down, which is exactly the state we want -- nothing more to do.
    if let Ok(mut conn) = connect_hello(sock).await {
        let _ = conn
            .write_frame(&hub_proto::encode_control(&ControlMsg::Shutdown))
            .await;
        // No reply is expected: the daemon closes the connection as its
        // implicit ack (mirrors the EOF-as-ack pattern above for Kill).
    }
    Ok(())
}
