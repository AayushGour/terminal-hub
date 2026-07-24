// Backend connection manager — one authenticated connection PER attached
// session (Approach A), replacing the earlier single-shared-connection actor.
//
// WHY per-tile connections: the real daemon's model is "one connection = one
// attached session". After a connection sends `Attach { id }`, the daemon's
// `drive_viewer_attached` (hub-daemon/src/server.rs) LOCKS that connection to
// `id`: every subsequent Input/`Kill`/`Resize`/`ClaimSize` is routed to `id`
// (the frame's own id is ignored), and `Detach` CLOSES the connection. So a
// single connection can neither view nor drive more than one session. The old
// "one shared `DaemonClient` multiplexing every session" only worked against
// the mock; against the real daemon only the first-attached tile would stream,
// `kill(X)` would hit the first-attached session, and `detach` would tear down
// the app's only connection. This module gives each open tile its OWN
// connection, mirroring `hub_tui::ViewerClient` and `hub_cli::daemon_client`.
//
// AUTH (F1): EVERY connection to the daemon — the persistent per-tile viewer
// connections AND the short-lived control connections (list / kill-a-ghost) —
// MUST send `ControlMsg::Hello { token }` as its very first frame or the
// daemon closes it before processing anything (hub_transport::auth). The token
// is loaded from `<base>/token`, where `base` is the daemon socket's parent
// dir. A missing token or a down/rejecting daemon surfaces as a clean `Err`
// (never a panic), so a command can report "daemon unavailable" and the tile
// can retry (re-`attach`) later.
//
// RECONNECT is per-tile: when a tile's connection dies (daemon crash / relay
// exit -> Closed/EOF) its reader emits `hub://closed` for that id and the
// entry is left as a finished task; a subsequent `attach(id)` replaces it and
// reopens. There is no app-wide connection to lose.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context};
use hub_proto::{encode_control, ControlMsg, Frame, SessionId, SessionInfo};
use hub_transport::{connect, FramedConn};
use tokio::sync::{mpsc, Mutex};

/// Bound on how long a short-lived `kill_oneshot` waits for the daemon/relay to
/// ack a `Kill` before treating the lack of a reply as best-effort success —
/// mirrors `hub_cli::daemon_client::KILL_ACK_TIMEOUT` exactly (the relay, and
/// therefore the daemon's bookkeeping, can tear down before it acks).
const KILL_ACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Abstracts webview event emission so the manager/reader are testable without
/// a Tauri runtime.
pub trait EventSink: Send + Sync + 'static {
    fn emit_json(&self, event: &'static str, payload: serde_json::Value);
}

/// Production sink: forwards to the Tauri webview.
pub struct AppSink(pub tauri::AppHandle);
impl EventSink for AppSink {
    fn emit_json(&self, event: &'static str, payload: serde_json::Value) {
        use tauri::Emitter; // v2: emit lives on the Emitter trait
        let _ = self.0.emit(event, payload);
    }
}

/// One authenticated connection dedicated to a single session: an mpsc sender
/// to write pre-encoded frames on it, plus the reader/writer actor task. The
/// actor terminates (closing the socket) when `input_tx` is dropped, so
/// removing a `ViewerConn` from the map — or dropping it — detaches that
/// viewer at the daemon (which never kills the shell on detach).
struct ViewerConn {
    input_tx: mpsc::Sender<Vec<u8>>,
    task: tokio::task::JoinHandle<()>,
}

impl ViewerConn {
    /// A `ViewerConn` whose actor has exited (connection dead) is stale: a
    /// re-`attach` must replace it rather than treat the session as still open.
    fn is_alive(&self) -> bool {
        !self.task.is_finished()
    }
}

/// Owns every per-tile connection. Cheap to construct (no I/O); connections are
/// opened lazily by `attach` and short-lived control ops.
pub struct ConnManager {
    /// `<base>/hubd.sock`.
    sock: PathBuf,
    /// The daemon socket's parent dir, where `<base>/token` lives.
    base: PathBuf,
    sink: Arc<dyn EventSink>,
    conns: Mutex<HashMap<u64, ViewerConn>>,
}

impl ConnManager {
    /// Build a manager targeting `sock` (`<base>/hubd.sock`). The auth token is
    /// read from `sock`'s parent dir — the same convention every other hub
    /// client uses (`hub_cli::daemon_client`, `hub_tui::ViewerClient`).
    pub fn new(sock: PathBuf, sink: Arc<dyn EventSink>) -> ConnManager {
        let base = sock
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        ConnManager { sock, base, sink, conns: Mutex::new(HashMap::new()) }
    }

    /// FIX 4 (hygiene): drop finished (`Closed`/EOF) entries from the map.
    /// Called only while `self.conns` is already locked by the caller, so
    /// there's no race with a concurrent `attach`'s insert/replace of the
    /// SAME id — the mutex serializes them. This never removes an entry a
    /// concurrent attach just inserted (that entry is alive by definition),
    /// only ones whose actor has already exited on its own; no fd/task leak
    /// either way, this is pure map bookkeeping so dead entries don't linger
    /// until an unrelated attach/detach happens to touch them.
    fn reap_dead(map: &mut HashMap<u64, ViewerConn>) {
        map.retain(|_, vc| vc.is_alive());
    }

    /// Open a NEW dedicated connection for `id`: `Hello{token}` -> `Attach{id}`,
    /// then spawn a reader/writer actor that emits this session's events. No-op
    /// if `id` already has a LIVE connection (idempotent); a dead entry is
    /// replaced (reconnect). Never opens two connections for the same id.
    pub async fn attach(&self, id: u64) -> anyhow::Result<()> {
        // Fast path: already attached and live. Reap other dead entries
        // while we hold the lock anyway (FIX 4).
        {
            let mut map = self.conns.lock().await;
            Self::reap_dead(&mut map);
            if map.get(&id).is_some_and(ViewerConn::is_alive) {
                return Ok(());
            }
        }

        // Open + authenticate + attach WITHOUT holding the map lock across I/O.
        let mut conn = connect_hello(&self.sock, &self.base)
            .await
            .with_context(|| format!("attach: connect+auth to daemon for session {id}"))?;
        conn.write_frame(&crate::commands::build_attach(id))
            .await
            .with_context(|| format!("attach: send Attach for session {id}"))?;

        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(256);
        let task = tokio::spawn(viewer_actor(conn, input_rx, id, self.sink.clone()));

        let mut map = self.conns.lock().await;
        if map.get(&id).is_some_and(ViewerConn::is_alive) {
            // Lost a race with a concurrent attach: keep the existing live
            // connection and let ours close (dropping input_tx stops its actor,
            // which drops the socket -> daemon detaches this redundant viewer).
            drop(input_tx);
            return Ok(());
        }
        map.insert(id, ViewerConn { input_tx, task });
        Ok(())
    }

    /// Clone the writer for `id`'s live connection, or `Err` if `id` is not
    /// attached (a dead/finished entry counts as not attached).
    async fn sender_for(&self, id: u64) -> anyhow::Result<mpsc::Sender<Vec<u8>>> {
        match self.conns.lock().await.get(&id) {
            Some(vc) if vc.is_alive() => Ok(vc.input_tx.clone()),
            _ => Err(anyhow!("session {id} is not attached")),
        }
    }

    /// Route input (a `Data` frame) to `id`'s own connection. Input implies an
    /// open tile, so a missing connection is a clean `Err`.
    pub async fn send_input(&self, id: u64, bytes: Vec<u8>) -> anyhow::Result<()> {
        let tx = self.sender_for(id).await?;
        tx.send(crate::commands::build_input(id, bytes))
            .await
            .map_err(|_| anyhow!("session {id} connection closed"))
    }

    /// Route a `Resize` to `id`'s own connection.
    pub async fn resize(&self, id: u64, cols: u16, rows: u16) -> anyhow::Result<()> {
        let tx = self.sender_for(id).await?;
        tx.send(crate::commands::build_resize(id, cols, rows))
            .await
            .map_err(|_| anyhow!("session {id} connection closed"))
    }

    /// Route a `ClaimSize` to `id`'s own connection.
    pub async fn claim_size(&self, id: u64, cols: u16, rows: u16) -> anyhow::Result<()> {
        let tx = self.sender_for(id).await?;
        tx.send(crate::commands::build_claim_size(id, cols, rows))
            .await
            .map_err(|_| anyhow!("session {id} connection closed"))
    }

    /// Stop viewing `id`: remove its connection and close it. The daemon
    /// detaches that viewer (it NEVER kills the shell on detach). Best-effort
    /// sends an explicit `Detach{id}` first so the daemon detaches promptly;
    /// dropping the `ViewerConn` then stops its actor, which drops the socket.
    pub async fn detach(&self, id: u64) -> anyhow::Result<()> {
        let vc = self.conns.lock().await.remove(&id);
        if let Some(vc) = vc {
            let _ = vc.input_tx.send(crate::commands::build_detach(id)).await;
            // drop(vc): input_tx dropped -> actor breaks -> FramedConn dropped
            // -> socket FIN -> daemon detaches (does not kill).
        }
        Ok(())
    }

    /// Kill `id`'s shell.
    ///
    /// If `id` is currently attached, the daemon requires Attach-then-Kill ON
    /// THE SAME CONNECTION — and its dedicated connection already Attached — so
    /// we just send `Kill{id}` on it. The relay dies, the daemon broadcasts
    /// `Closed`, and this session's reader emits `hub://closed` before the
    /// actor exits.
    ///
    /// If `id` is NOT attached (killing a ghost/orphan from the list), open a
    /// SHORT-LIVED connection and do `Hello -> Attach{id} -> Kill{id} -> drain`
    /// (mirrors `hub_cli::daemon_client::kill_session` exactly).
    pub async fn kill(&self, id: u64) -> anyhow::Result<()> {
        if let Ok(tx) = self.sender_for(id).await {
            return tx
                .send(crate::commands::build_kill(id))
                .await
                .map_err(|_| anyhow!("session {id} connection closed"));
        }
        kill_oneshot(&self.sock, &self.base, SessionId(id)).await
    }

    /// List every session the daemon knows about over a SHORT-LIVED connection:
    /// `Hello -> List -> await Sessions -> close`. Never reuses a persistent
    /// viewer connection (mirrors `hub_cli::daemon_client::list_sessions`).
    pub async fn list_sessions(&self) -> anyhow::Result<Vec<SessionInfo>> {
        // FIX 4 (hygiene): this is on `App.svelte`'s periodic reconcile poll
        // (every 5s), so it's a convenient opportunistic point to reap dead
        // `ViewerConn` entries too — see `reap_dead`.
        Self::reap_dead(&mut *self.conns.lock().await);

        let mut conn = connect_hello(&self.sock, &self.base).await?;
        conn.write_frame(&encode_control(&ControlMsg::List)).await?;
        match conn.read_frame().await? {
            Frame::Control(ControlMsg::Sessions { sessions }) => Ok(sessions),
            Frame::Control(ControlMsg::Error { message }) => Err(anyhow!(message)),
            other => Err(anyhow!("unexpected reply to List: {other:?}")),
        }
    }
}

/// Client-side F1 dial shared by every connection this manager opens: connect,
/// then send the mandatory `Hello { token }` first frame. The token is loaded
/// (not lazily created) from `<base>/token`; a missing token or a
/// down/rejecting daemon returns `Err` here rather than panicking.
async fn connect_hello(sock: &Path, base: &Path) -> anyhow::Result<FramedConn> {
    let token = hub_transport::auth::load_token(base)?;
    let mut conn = connect(sock).await?;
    conn.send_hello(&token).await?;
    Ok(conn)
}

/// Short-lived `Kill`: `Attach{id} -> Kill{id}` on one authenticated
/// connection, then drain until a definitive `Closed`/`Error`, EOF, or timeout.
/// A close or timeout is best-effort success (the relay may exit before it
/// acks). Byte-for-byte the same handshake as
/// `hub_cli::daemon_client::kill_session`.
async fn kill_oneshot(sock: &Path, base: &Path, id: SessionId) -> anyhow::Result<()> {
    let mut conn = connect_hello(sock, base).await?;
    conn.write_frame(&encode_control(&ControlMsg::Attach { id })).await?;
    conn.write_frame(&encode_control(&ControlMsg::Kill { id })).await?;
    loop {
        match tokio::time::timeout(KILL_ACK_TIMEOUT, conn.read_frame()).await {
            Ok(Ok(Frame::Control(ControlMsg::Closed { id: cid, .. }))) if cid == id => return Ok(()),
            Ok(Ok(Frame::Control(ControlMsg::Error { message }))) => return Err(anyhow!(message)),
            Ok(Ok(_)) => continue, // Replay/Data from the attach; keep draining.
            Ok(Err(_)) => return Ok(()), // EOF -> best-effort success.
            Err(_) => return Ok(()),     // timeout -> best-effort success.
        }
    }
}

/// The per-connection actor: owns ONE `FramedConn` dedicated to session `id`.
/// Writes queued frames (input/control) and turns inbound daemon frames into
/// this session's Tauri events. The connection is bound to `id` at the daemon,
/// so every event is keyed on `id` (the authoritative routing key) rather than
/// the frame's own declared id, matching how the frontend filters by session.
async fn viewer_actor(
    mut conn: FramedConn,
    mut input_rx: mpsc::Receiver<Vec<u8>>,
    id: u64,
    sink: Arc<dyn EventSink>,
) {
    loop {
        tokio::select! {
            biased;
            // Writes drain first so keystrokes are never starved by reads, and
            // a Detach queued right before shutdown flushes before we close.
            maybe = input_rx.recv() => match maybe {
                Some(bytes) => { if conn.write_frame(&bytes).await.is_err() { break; } }
                None => break, // ViewerConn dropped (detach / replaced): close.
            },
            frame = conn.read_frame() => match frame {
                Ok(Frame::Control(ControlMsg::Replay { screen, .. })) => {
                    sink.emit_json("hub://replay", serde_json::json!({ "id": id, "bytes": screen }));
                }
                Ok(Frame::Data { bytes, .. }) => {
                    sink.emit_json("hub://output", serde_json::json!({ "id": id, "bytes": bytes }));
                }
                Ok(Frame::Control(ControlMsg::Closed { exit_code, .. })) => {
                    sink.emit_json("hub://closed", serde_json::json!({ "id": id, "exitCode": exit_code }));
                    break; // session ended; the daemon closes this connection.
                }
                Ok(Frame::Control(ControlMsg::Error { message })) => {
                    // e.g. "no session <id>" — the daemon will close next; keep
                    // reading until EOF ends us.
                    sink.emit_json("hub://error", serde_json::json!({ "id": id, "message": message }));
                }
                Ok(_) => { /* Open/Opened/Attach/etc. are not inbound to a viewer; ignore. */ }
                Err(_) => {
                    // EOF / connection died (daemon crash, relay exit w/o Closed).
                    // Surface as a close for this tile so it can re-attach.
                    sink.emit_json("hub://closed", serde_json::json!({ "id": id, "exitCode": serde_json::Value::Null }));
                    break;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hub_proto::{encode_control, encode_data, ControlMsg, Frame, Origin, SessionId, SessionInfo};
    use std::sync::Mutex as StdMutex;
    use tokio::net::UnixListener;

    /// Collecting sink: every emitted (event, payload) is appended so tests can
    /// poll for the events a session's reader produced. No Tauri runtime.
    #[derive(Clone)]
    struct VecSink(Arc<StdMutex<Vec<(String, serde_json::Value)>>>);
    impl EventSink for VecSink {
        fn emit_json(&self, event: &'static str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event.to_string(), payload));
        }
    }
    fn vec_sink() -> (Arc<dyn EventSink>, Arc<StdMutex<Vec<(String, serde_json::Value)>>>) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        (Arc::new(VecSink(events.clone())), events)
    }

    /// Set up a temp `<dir>/hubd.sock` bound listener + a matching token file,
    /// returning the socket path and the listener. The manager derives the
    /// token dir from the socket's parent, so no process-global env is touched.
    async fn setup() -> (tempfile::TempDir, PathBuf, UnixListener, String) {
        let dir = tempfile::tempdir().unwrap();
        let token = hub_transport::auth::ensure_token(dir.path()).unwrap();
        let sock = dir.path().join("hubd.sock");
        let listener = hub_transport::bind_listener(&sock).await.unwrap();
        (dir, sock, listener, token)
    }

    async fn read_hello(conn: &mut FramedConn, token: &str) {
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::Hello { token: got }) => assert_eq!(got, token),
            other => panic!("expected Hello first, got {other:?}"),
        }
    }

    /// Poll the collected events until `pred` is satisfied or ~2s elapses.
    async fn wait_until<F: Fn(&[(String, serde_json::Value)]) -> bool>(
        events: &Arc<StdMutex<Vec<(String, serde_json::Value)>>>,
        pred: F,
    ) -> bool {
        for _ in 0..200 {
            if pred(&events.lock().unwrap()) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    fn has_output(events: &[(String, serde_json::Value)], id: u64, needle: &[u8]) -> bool {
        events.iter().any(|(ev, p)| {
            ev == "hub://output"
                && p["id"] == id
                && p["bytes"]
                    .as_array()
                    .map(|a| a.iter().map(|n| n.as_u64().unwrap() as u8).collect::<Vec<u8>>())
                    .map(|b| b.windows(needle.len().max(1)).any(|w| w == needle))
                    .unwrap_or(false)
        })
    }

    // A per-connection Attach makes THAT session's Data arrive as hub://output.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attach_streams_that_sessions_output() {
        let (_dir, sock, listener, token) = setup().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            match conn.read_frame().await.unwrap() {
                Frame::Control(ControlMsg::Attach { id }) => assert_eq!(id, SessionId(5)),
                other => panic!("expected Attach, got {other:?}"),
            }
            conn.write_frame(&encode_data(SessionId(5), b"out5")).await.unwrap();
            tokio::time::sleep(Duration::from_millis(300)).await; // keep open
        });

        let (sink, events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(5).await.unwrap();
        assert!(wait_until(&events, |e| has_output(e, 5, b"out5")).await, "expected hub://output for id 5");
        server.await.unwrap();
    }

    // TWO simultaneous attaches each open their OWN connection and stream only
    // their own session's output — the per-tile isolation the mock could hide.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_attaches_are_independent_connections() {
        let (_dir, sock, listener, token) = setup().await;
        // Accept exactly two connections; each learns its Attach id and echoes
        // a distinct payload tagged with that id.
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let token = token.clone();
                tokio::spawn(async move {
                    let mut conn = FramedConn::new(stream);
                    read_hello(&mut conn, &token).await;
                    let id = match conn.read_frame().await.unwrap() {
                        Frame::Control(ControlMsg::Attach { id }) => id,
                        other => panic!("expected Attach, got {other:?}"),
                    };
                    let payload = format!("sess{}", id.0);
                    conn.write_frame(&encode_data(id, payload.as_bytes())).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(300)).await;
                });
            }
        });

        let (sink, events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(1).await.unwrap();
        mgr.attach(2).await.unwrap();
        assert!(wait_until(&events, |e| has_output(e, 1, b"sess1") && has_output(e, 2, b"sess2")).await,
            "each session must stream its OWN output independently");
        // And never crossed: id 1 must not carry sess2's bytes and vice versa.
        let e = events.lock().unwrap();
        assert!(!has_output(&e, 1, b"sess2"), "session 1 must not receive session 2's output");
        assert!(!has_output(&e, 2, b"sess1"), "session 2 must not receive session 1's output");
        server.await.unwrap();
    }

    // Input is written on THAT session's own connection as a Data frame.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn send_input_writes_on_that_sessions_connection() {
        let (_dir, sock, listener, token) = setup().await;
        let (got_tx, got_rx) = std::sync::mpsc::channel::<Frame>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            // Attach, then the input Data frame.
            let _ = conn.read_frame().await.unwrap(); // Attach
            let input = conn.read_frame().await.unwrap();
            got_tx.send(input).unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(9).await.unwrap();
        mgr.send_input(9, b"hi".to_vec()).await.unwrap();
        let input = got_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(input, Frame::Data { id: SessionId(9), bytes: b"hi".to_vec() });
        server.await.unwrap();
    }

    // Input to a session that was never attached is a clean Err (no socket I/O).
    #[tokio::test]
    async fn send_input_errs_when_not_attached() {
        let (_dir, sock, _listener, _token) = setup().await;
        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        assert!(mgr.send_input(42, b"x".to_vec()).await.is_err());
    }

    // list_sessions uses a SHORT-LIVED Hello -> List -> Sessions connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_uses_short_lived_connection() {
        let (_dir, sock, listener, token) = setup().await;
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            match conn.read_frame().await.unwrap() {
                Frame::Control(ControlMsg::List) => {}
                other => panic!("expected List, got {other:?}"),
            }
            let info = SessionInfo {
                id: SessionId(3), origin: Origin::Hub, title: "t".into(),
                pid: 1, started_unix: 1, cols: 80, rows: 24,
                cwd: String::new(), last_exit_code: None, activity_seq: 0,
            };
            conn.write_frame(&encode_control(&ControlMsg::Sessions { sessions: vec![info] })).await.unwrap();
        });

        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        let sessions = mgr.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, SessionId(3));
        server.await.unwrap();
    }

    // kill of a NON-attached id opens a short-lived Attach->Kill connection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_not_attached_does_attach_then_kill_on_short_lived_conn() {
        let (_dir, sock, listener, token) = setup().await;
        let (seq_tx, seq_rx) = std::sync::mpsc::channel::<Frame>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            seq_tx.send(conn.read_frame().await.unwrap()).unwrap(); // Attach
            seq_tx.send(conn.read_frame().await.unwrap()).unwrap(); // Kill
            conn.write_frame(&encode_control(&ControlMsg::Closed { id: SessionId(8), exit_code: Some(0) })).await.unwrap();
        });

        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.kill(8).await.unwrap();
        assert_eq!(seq_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Frame::Control(ControlMsg::Attach { id: SessionId(8) }));
        assert_eq!(seq_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Frame::Control(ControlMsg::Kill { id: SessionId(8) }));
        server.await.unwrap();
    }

    // kill of an ATTACHED id sends Kill on the SAME connection (no 2nd connect).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn kill_attached_sends_kill_on_same_connection() {
        let (_dir, sock, listener, token) = setup().await;
        let (seq_tx, seq_rx) = std::sync::mpsc::channel::<Frame>();
        let server = tokio::spawn(async move {
            // Only ONE connection is accepted for the whole test.
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            seq_tx.send(conn.read_frame().await.unwrap()).unwrap(); // Attach
            seq_tx.send(conn.read_frame().await.unwrap()).unwrap(); // Kill on SAME conn
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(4).await.unwrap();
        mgr.kill(4).await.unwrap();
        assert_eq!(seq_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Frame::Control(ControlMsg::Attach { id: SessionId(4) }));
        assert_eq!(seq_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
            Frame::Control(ControlMsg::Kill { id: SessionId(4) }));
        server.await.unwrap();
    }

    // detach sends a Detach frame then closes the connection (never kills).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn detach_sends_detach_then_closes() {
        let (_dir, sock, listener, token) = setup().await;
        let (obs_tx, obs_rx) = std::sync::mpsc::channel::<&'static str>();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut conn = FramedConn::new(stream);
            read_hello(&mut conn, &token).await;
            let _ = conn.read_frame().await.unwrap(); // Attach
            match conn.read_frame().await.unwrap() {
                Frame::Control(ControlMsg::Detach { id }) => {
                    assert_eq!(id, SessionId(6));
                    obs_tx.send("detach").unwrap();
                }
                other => panic!("expected Detach, got {other:?}"),
            }
            // After detach, the client drops the connection -> we observe EOF.
            assert!(conn.read_frame().await.is_err(), "connection must close after detach");
            obs_tx.send("closed").unwrap();
        });

        let (sink, _events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(6).await.unwrap();
        mgr.detach(6).await.unwrap();
        assert_eq!(obs_rx.recv_timeout(Duration::from_secs(2)).unwrap(), "detach");
        assert_eq!(obs_rx.recv_timeout(Duration::from_secs(2)).unwrap(), "closed");
        server.await.unwrap();
    }

    // FIX 4: direct unit test of the reap helper -- only a finished-task
    // entry is removed; a live one is left untouched.
    #[tokio::test]
    async fn reap_dead_removes_only_finished_entries() {
        let (dead_tx, _dead_rx) = mpsc::channel::<Vec<u8>>(1);
        let dead_task = tokio::spawn(async {});
        for _ in 0..200 {
            if dead_task.is_finished() { break; }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(dead_task.is_finished(), "setup: dead_task should have finished by now");

        let (alive_tx, _alive_rx) = mpsc::channel::<Vec<u8>>(1);
        let alive_task = tokio::spawn(async { tokio::time::sleep(Duration::from_secs(60)).await; });

        let mut map: HashMap<u64, ViewerConn> = HashMap::new();
        map.insert(1, ViewerConn { input_tx: dead_tx, task: dead_task });
        map.insert(2, ViewerConn { input_tx: alive_tx, task: alive_task });

        ConnManager::reap_dead(&mut map);

        assert!(!map.contains_key(&1), "dead entry must be reaped");
        assert!(map.contains_key(&2), "live entry must be left alone");
    }

    // FIX 4 (end-to-end): a dead entry left behind by id 1's connection dying
    // is reaped the next time `attach` locks the map -- even when that call
    // is for a completely unrelated id (2). It doesn't linger until some
    // future attach/detach of id 1 itself happens to touch it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attach_reaps_a_dead_entry_left_by_a_different_id() {
        let (_dir, sock, listener, token) = setup().await;
        let server = tokio::spawn(async move {
            // First connection (id 1): Hello+Attach, then drop -> EOF at the client.
            let (stream, _) = listener.accept().await.unwrap();
            {
                let mut conn = FramedConn::new(stream);
                read_hello(&mut conn, &token).await;
                let _ = conn.read_frame().await.unwrap(); // Attach{1}
                // conn (and its stream) drop here, closing the socket.
            }
            // Second connection (id 2): stay open so it counts as alive.
            let (stream2, _) = listener.accept().await.unwrap();
            let mut conn2 = FramedConn::new(stream2);
            read_hello(&mut conn2, &token).await;
            let _ = conn2.read_frame().await.unwrap(); // Attach{2}
            tokio::time::sleep(Duration::from_millis(300)).await; // keep open
        });

        let (sink, events) = vec_sink();
        let mgr = ConnManager::new(sock, sink);
        mgr.attach(1).await.unwrap();
        assert!(
            wait_until(&events, |e| e.iter().any(|(ev, p)| ev == "hub://closed" && p["id"] == 1)).await,
            "expected hub://closed once session 1's connection dies"
        );
        // Wait for the actor task to actually finish (is_finished() reflects that).
        for _ in 0..200 {
            let dead = mgr.conns.lock().await.get(&1).is_some_and(|vc| !vc.is_alive());
            if dead { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            mgr.conns.lock().await.get(&1).is_some_and(|vc| !vc.is_alive()),
            "id 1's entry should be dead (but still present) before the next attach reaps it"
        );

        // A subsequent attach for an UNRELATED id (2) reaps the dead id-1 entry.
        mgr.attach(2).await.unwrap();
        assert!(mgr.conns.lock().await.get(&1).is_none(), "dead entry for id 1 should have been reaped");
        assert!(
            mgr.conns.lock().await.get(&2).is_some_and(|vc| vc.is_alive()),
            "id 2 should be attached and alive"
        );
        server.await.unwrap();
    }
}
