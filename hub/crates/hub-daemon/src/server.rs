use crate::registry::{Registry, ViewerSink, VIEWER_CHANNEL_BOUND};
use hub_proto::SessionId as SessionInfoId;
use hub_proto::{encode_control, ControlMsg, Frame, SessionInfo};
use hub_relay::conn::{write_frame, FrameReader};
use hub_relay::paths::HubPaths;
use std::os::unix::io::AsRawFd;
use tokio::net::unix::OwnedWriteHalf;
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub async fn run(paths: HubPaths) -> anyhow::Result<()> {
    paths.ensure_dirs()?;

    // F1 auth: ensure the per-install secret token exists BEFORE we bind the
    // socket, so it is present the instant any client can connect. Lazily
    // created here (32 bytes of OS randomness, 0600) if `hub install` didn't,
    // so e2e/tests without a full install still work. Held for the daemon's
    // lifetime; loaded once and shared with every connection handler.
    let token = hub_transport::auth::ensure_token(paths.base())?;

    // Singleton guard (contract §J): only the exclusive flock-holder may
    // reach `bind_listener`, which unconditionally unlinks a stale
    // `hubd.sock`. Without this, a 2nd daemon started under the same
    // HUB_DIR would silently steal the socket from a live one. Held for the
    // whole daemon lifetime (dropped, releasing the lock, on process exit).
    let lock_path = paths.base().join("hubd.lock");
    let _singleton = match crate::singleton::acquire(&lock_path) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("hub-daemon: daemon already running ({e:#})");
            return Err(e);
        }
    };

    let reg = Registry::default();

    // Reconcile: adopt healthy relays, prune ghosts, log orphans.
    let buckets = crate::reconcile::reconcile(&paths).await;
    let mut max_id = 0u64;
    for b in buckets {
        match b {
            crate::reconcile::Bucket::Healthy(info) => {
                max_id = max_id.max(info.id.0);
                adopt_relay(&reg, &paths, info, &token).await; // dial <id>.sock as routing channel
            }
            crate::reconcile::Bucket::Ghost(id) => {
                // Ghost ids must still raise the floor: the record's id may
                // still be referenced elsewhere even though we're about to
                // prune it, and a stale/slow-to-die socket could still be
                // holding the id below.
                max_id = max_id.max(id.0);
                tracing::info!("pruning ghost session {}", id.0);
                crate::reconcile::prune_ghost(&paths, id);
            }
            crate::reconcile::Bucket::Orphan(sock) => {
                // Orphan: live socket, no record. Its id must never be
                // reassigned to a new Open, or we'd remove_file + rebind a
                // socket a live relay is still holding open (id-hijack).
                if let Some(id) = parse_session_id_from_sock(&sock) {
                    max_id = max_id.max(id);
                }
                tracing::info!("orphan live socket with no record: {:?}", sock);
                // v1: surfaced for the CLI/GUI to adopt/kill; daemon leaves it be.
            }
        }
    }
    reg.bump_next_id_at_least(max_id).await;

    let listener = hub_transport::bind_listener(&paths.daemon_sock()).await?;
    tracing::info!("hub-daemon listening at {:?}", paths.daemon_sock());

    // Signaled by a connection handler on `ControlMsg::Shutdown` to break the
    // accept loop and let `run()` return, exiting the process cleanly. This
    // NEVER touches relay connections/tasks -- relays own the ptys and are
    // independent processes by design (SPOF architecture), so they simply
    // keep running after the daemon detaches and exits.
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let reg = reg.clone();
                let paths = paths.clone();
                let shutdown_tx = shutdown_tx.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(stream, reg, paths, shutdown_tx, token).await {
                        tracing::warn!("conn ended: {e:#}");
                    }
                });
            }
            _ = shutdown_rx.recv() => {
                tracing::info!("Shutdown received: daemon exiting; relays are left running (SPOF design)");
                break;
            }
        }
    }
    Ok(())
}

/// Parse the numeric session id out of a `sessions/<id>.sock` path.
fn parse_session_id_from_sock(sock: &std::path::Path) -> Option<u64> {
    sock.file_stem()?.to_str()?.parse().ok()
}

/// Dial a live relay's per-session socket and use it as the routing channel
/// (daemon acts as the client after a restart): split the stream, spawn the
/// writer sink as `relay_tx`, register the session, and drive relay frames.
async fn adopt_relay(reg: &Registry, paths: &HubPaths, info: SessionInfo, token: &str) {
    let sock = paths.sock(info.id);
    match tokio::net::UnixStream::connect(&sock).await {
        Ok(stream) => {
            let (rd, mut wr) = stream.into_split();
            // F1: authenticate to the relay's per-session socket first (the
            // relay treats the daemon like any other client). Send Hello
            // BEFORE anything else, or the relay rejects the adoption dial.
            if let Err(e) = hub_transport::auth::send_hello(&mut wr, token).await {
                tracing::warn!("adopt auth failed for {}: {e:#}", info.id.0);
                return;
            }
            let relay_tx = spawn_writer(wr);
            reg.add_session(info.clone(), relay_tx).await;
            let reg2 = reg.clone();
            let id = info.id;
            tokio::spawn(async move {
                let mut fr = FrameReader::new(rd);
                let _ = drive_relay(&mut fr, &reg2, id).await;
            });
            tracing::info!("adopted relay session {}", info.id.0);
        }
        Err(e) => tracing::warn!("adopt failed for {}: {e:#}", info.id.0),
    }
}

/// Spawn a writer task draining `rx` to the socket write half; return its sender.
/// UNBOUNDED — used ONLY for relay-facing connections (shell-never-stalls).
fn spawn_writer(mut wr: OwnedWriteHalf) -> mpsc::UnboundedSender<Vec<u8>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if write_frame(&mut wr, &bytes).await.is_err() { break; }
        }
    });
    tx
}

/// Spawn a BOUNDED writer task for a single VIEWER connection and return a
/// `ViewerSink` (I3/F4). The bounded channel is the backpressure point: when the
/// viewer stops reading, the socket write blocks, the channel fills, and the
/// fan-out's `try_send` then fails so the viewer is dropped (see `fan_out_output`).
///
/// Dropping the returned `ViewerSink` (i.e. removing the viewer from the
/// registry) drops the oneshot sender; the writer task observes that and
/// `shutdown()`s the socket (FIN), so the client sees its connection close and
/// reconnects + Replay. A blocked `write_frame` (viewer not reading) is
/// cancelled by the same signal, so eviction is prompt and never hangs.
fn spawn_viewer_writer(mut wr: OwnedWriteHalf) -> ViewerSink {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(VIEWER_CHANNEL_BOUND);
    let (kill_tx, mut kill_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                // Removal (ViewerSink dropped) -> shut the socket down, even if a
                // write is in flight to a viewer that stopped reading.
                _ = &mut kill_rx => break,
                step = async {
                    match rx.recv().await {
                        Some(bytes) => write_frame(&mut wr, &bytes).await.is_ok(),
                        None => false, // sender gone
                    }
                } => { if !step { break; } }
            }
        }
        use tokio::io::AsyncWriteExt;
        let _ = wr.shutdown().await;
    });
    ViewerSink { tx, _kill: kill_tx, behind_since: None }
}

pub async fn handle_conn(
    stream: UnixStream,
    reg: Registry,
    paths: HubPaths,
    shutdown_tx: mpsc::Sender<()>,
    token: String,
) -> anyhow::Result<()> {
    // F1 auth gate. Peer-uid check first (defense in depth) on the raw fd,
    // which we still hold before splitting. Then require a valid Hello as the
    // FIRST frame. Any failure => log a rejection WITHOUT the token and close
    // the connection, processing nothing. This runs per-connection inside a
    // spawned task, so the bounded Hello read can never stall the accept loop.
    if !hub_transport::auth::peer_uid_ok(stream.as_raw_fd()) {
        tracing::warn!("rejected connection: peer uid mismatch");
        return Ok(());
    }
    let (rd, wr) = stream.into_split();
    let mut fr = FrameReader::new(rd);
    if let Err(e) = fr.verify_hello(&token).await {
        tracing::warn!("rejected connection: {e}");
        return Ok(());
    }

    // Defer the writer sink until we know whether this is a relay (unbounded,
    // shell-never-stalls) or a viewer (bounded, drop-slow-viewer). Reading the
    // first frame only needs the read half.
    let first = match fr.next().await? { Some(f) => f, None => return Ok(()) };
    match first {
        Frame::Control(ControlMsg::Open { origin, title, cols, rows, .. }) => {
            let tx = spawn_writer(wr); // relay-facing: UNBOUNDED
            let id = reg.alloc_id().await;
            let info = SessionInfo {
                id, origin, title, pid: 0, started_unix: now_unix(), cols, rows,
                cwd: String::new(), last_exit_code: None, activity_seq: 0,
            };
            reg.add_session(info, tx.clone()).await;
            let _ = tx.send(encode_control(&ControlMsg::Opened { id }));
            tracing::info!("relay registered as session {}", id.0);
            // Pick up the relay's real pid from its record (Open registers pid: 0).
            spawn_pid_pickup(reg.clone(), paths.clone(), id);
            drive_relay(&mut fr, &reg, id).await
        }
        Frame::Control(ControlMsg::List) => {
            let sink = spawn_viewer_writer(wr); // viewer-facing: BOUNDED
            let _ = sink.tx.try_send(encode_control(&ControlMsg::Sessions { sessions: reg.list().await }));
            drive_viewer(&mut fr, &reg, sink).await
        }
        Frame::Control(ControlMsg::Attach { id }) => {
            let sink = spawn_viewer_writer(wr); // viewer-facing: BOUNDED
            match reg.attach_viewer(id, sink).await {
                Ok(vid) => drive_viewer_attached(&mut fr, &reg, id, vid).await,
                Err(sink) => { let _ = sink.tx.try_send(encode_control(&ControlMsg::Error { message: format!("no session {}", id.0) })); Ok(()) }
            }
        }
        Frame::Control(ControlMsg::Shutdown) => {
            // Graceful daemon-process stop (contract §J / ADDENDUM F). We do
            // NOT touch the registry's relay connections here -- relays own
            // the ptys and must survive daemon death (SPOF design). Signal
            // the accept loop to break so `run()` returns and the process
            // exits cleanly; closing this connection (by returning) is the
            // client's ack (mirrors the EOF-as-ack pattern `kill_session`
            // already treats as best-effort success).
            tracing::info!("Shutdown requested; daemon will exit, relays are unaffected");
            let _ = shutdown_tx.try_send(());
            Ok(())
        }
        other => {
            let kind = match &other {
                Frame::Control(_) => "control",
                Frame::Data { .. } => "data",
            };
            tracing::warn!("unexpected first frame: {}", kind);
            Ok(())
        }
    }
}

/// Poll briefly for the relay's session record and copy its real pid into the
/// registry — Open registers with `pid: 0` before the relay writes its record.
fn spawn_pid_pickup(reg: Registry, paths: HubPaths, id: SessionInfoId) {
    tokio::spawn(async move {
        for _ in 0..50 {
            if let Ok(rec) = hub_relay::record::SessionRecord::load(&paths.record(id)) {
                reg.set_pid(id, rec.pid).await;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    });
}

/// Relay channel driver: fan out Output, deliver Replay, broadcast Closed.
///
/// M1: this task is bound to ONE relay connection for a single KNOWN session
/// `id` (its caller either just allocated it via `Open` or is re-adopting a
/// specific record via `adopt_relay`). Frames arriving here nonetheless carry
/// their OWN `id` field (part of the wire format shared with viewer-facing
/// frames), and a buggy or hostile relay could declare a DIFFERENT session's
/// id in a `Data`/`Replay`/`Closed` frame. Trusting that declared id would let
/// one relay's connection evict or fan-out into a completely different
/// session in the registry -- a cross-session isolation break. So every
/// registry action below is keyed on the connection's own `id`, never on the
/// frame's; a frame that declares a foreign id is logged and dropped instead
/// of acted on. This mirrors the same fix already applied to viewer input
/// routing (`drive_viewer_attached` uses the attached id, not a frame id).
async fn drive_relay(fr: &mut FrameReader<tokio::net::unix::OwnedReadHalf>, reg: &Registry, id: SessionInfoId) -> anyhow::Result<()> {
    while let Some(frame) = fr.next().await? {
        match frame {
            Frame::Data { id: fid, bytes } => {
                if fid != id {
                    tracing::warn!("drive_relay({}): dropping Data frame declaring foreign id {}", id.0, fid.0);
                    continue;
                }
                reg.fan_out_output(id, &bytes).await
            }
            Frame::Control(ControlMsg::Replay { id: rid, screen }) => {
                if rid != id {
                    tracing::warn!("drive_relay({}): dropping Replay frame declaring foreign id {}", id.0, rid.0);
                    continue;
                }
                reg.deliver_replay(id, screen).await
            }
            Frame::Control(ControlMsg::Closed { id: cid, exit_code }) => {
                if cid != id {
                    tracing::warn!("drive_relay({}): dropping Closed frame declaring foreign id {}", id.0, cid.0);
                    continue;
                }
                reg.broadcast_closed(id, exit_code).await;
                break;
            }
            Frame::Control(ControlMsg::SessionActivity { id: aid, cwd, last_exit_code, activity_seq }) => {
                if aid != id {
                    tracing::warn!("drive_relay({}): dropping SessionActivity frame declaring foreign id {}", id.0, aid.0);
                    continue;
                }
                reg.update_activity(id, cwd, last_exit_code, activity_seq).await;
            }
            _ => {}
        }
    }
    reg.broadcast_closed(id, None).await; // relay disconnected without Closed
    Ok(())
}

/// Viewer that only listed (may later Attach on the same conn). Holds the
/// bounded `ViewerSink` until an Attach hands it to the registry (or the conn
/// ends, dropping it and closing the socket).
async fn drive_viewer(fr: &mut FrameReader<tokio::net::unix::OwnedReadHalf>, reg: &Registry, sink: ViewerSink) -> anyhow::Result<()> {
    while let Some(frame) = fr.next().await? {
        if let Frame::Control(ControlMsg::Attach { id }) = frame {
            return match reg.attach_viewer(id, sink).await {
                Ok(vid) => drive_viewer_attached(fr, reg, id, vid).await,
                Err(_sink) => Ok(()), // no such session; sink dropped -> socket closes
            };
        }
    }
    Ok(())
}

/// Attached viewer: route Input/Resize/ClaimSize/Kill to relay; Detach removes.
async fn drive_viewer_attached(fr: &mut FrameReader<tokio::net::unix::OwnedReadHalf>, reg: &Registry, id: SessionInfoId, vid: u64) -> anyhow::Result<()> {
    let res = async {
        while let Some(frame) = fr.next().await? {
            match frame {
                Frame::Data { bytes, .. } => reg.route_input(id, bytes).await,
                Frame::Control(m @ ControlMsg::Resize { .. }) => reg.route_control_to_relay(id, &m).await,
                Frame::Control(m @ ControlMsg::ClaimSize { .. }) => reg.route_control_to_relay(id, &m).await,
                Frame::Control(m @ ControlMsg::Kill { .. }) => reg.route_control_to_relay(id, &m).await,
                Frame::Control(ControlMsg::Detach { .. }) => break,
                _ => {}
            }
        }
        anyhow::Ok(())
    }.await;
    reg.detach_viewer(id, vid).await; // viewer gone -> detach, never kill
    res
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0)
}
