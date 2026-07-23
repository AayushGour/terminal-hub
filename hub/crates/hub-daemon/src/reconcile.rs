//! Startup two-scan reconciliation (spec §9): auto-discover which per-session
//! sockets actually answer, diff against record files, bucket into
//! healthy / ghost / orphan.

use hub_proto::{encode_control, ControlMsg, Frame, SessionId, SessionInfo};
use hub_relay::conn::{write_frame, FrameReader};
use hub_relay::paths::HubPaths;
use hub_relay::record::SessionRecord;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug)]
pub enum Bucket {
    /// Record present AND socket answers — attach.
    Healthy(SessionInfo),
    /// Record present but socket dead (relay crashed) — offer cleanup.
    Ghost(SessionId),
    /// Socket answers but no record — offer reconnect/adopt/kill.
    Orphan(PathBuf),
}

/// Connect to a per-session socket, send List, expect a Sessions reply within
/// a short timeout. Returns the reported SessionInfo if live.
///
/// F1: the relay's per-session socket now requires auth, so the liveness probe
/// must authenticate too — send `Hello { token }` as the FIRST frame before
/// `List`. The token lives at `<base>/token` where `base` is the hub dir two
/// levels up from `<base>/sessions/<id>.sock`; `ensure_token` reads the
/// daemon-created token (or creates one in the standalone-reconcile-test case).
pub async fn probe_live(sock: &Path) -> Option<SessionInfo> {
    let base = sock.parent().and_then(|p| p.parent())?;
    let token = hub_transport::auth::ensure_token(base).ok()?;
    let fut = async {
        let stream = tokio::net::UnixStream::connect(sock).await.ok()?;
        let (rd, mut wr) = stream.into_split();
        let mut fr = FrameReader::new(rd);
        hub_transport::auth::send_hello(&mut wr, &token).await.ok()?;
        write_frame(&mut wr, &encode_control(&ControlMsg::List)).await.ok()?;
        match fr.next().await.ok()?? {
            Frame::Control(ControlMsg::Sessions { sessions }) => sessions.into_iter().next(),
            _ => None,
        }
    };
    tokio::time::timeout(Duration::from_millis(300), fut).await.ok().flatten()
}

pub async fn reconcile(paths: &HubPaths) -> Vec<Bucket> {
    let dir = paths.sessions_dir();
    let mut records: Vec<SessionRecord> = vec![];
    let mut live_socks: Vec<PathBuf> = vec![];

    if let Ok(rd) = std::fs::read_dir(&dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            match p.extension().and_then(|s| s.to_str()) {
                Some("json") => if let Ok(r) = SessionRecord::load(&p) { records.push(r); },
                Some("sock") => live_socks.push(p),
                _ => {}
            }
        }
    }

    let mut buckets = vec![];
    let mut matched_socks: HashSet<PathBuf> = HashSet::new();

    // Records -> healthy or ghost.
    for rec in &records {
        let sock = paths.sock(rec.id);
        if sock.exists() && probe_live(&sock).await.is_some() {
            matched_socks.insert(sock);
            buckets.push(Bucket::Healthy(rec.to_info()));
        } else {
            buckets.push(Bucket::Ghost(rec.id));
        }
    }

    // Live sockets with no matching record -> orphan.
    for sock in live_socks {
        if matched_socks.contains(&sock) { continue; }
        if probe_live(&sock).await.is_some() {
            buckets.push(Bucket::Orphan(sock));
        }
    }
    buckets
}

/// Prune a ghost: delete its record (socket already gone).
pub fn prune_ghost(paths: &HubPaths, id: SessionId) {
    SessionRecord::delete(paths, id);
    let _ = std::fs::remove_file(paths.sock(id));
}
