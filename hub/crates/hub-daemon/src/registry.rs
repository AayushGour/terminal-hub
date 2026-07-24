use hub_proto::{encode_control, encode_data, ControlMsg, SessionId, SessionInfo};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::Mutex;

/// Relay-facing sink (daemon -> relay). UNBOUNDED on purpose: the shell must
/// NEVER stall (spec: shell-never-stalls). Input/Attach/Detach frames are tiny
/// and low-rate, so unboundedness here is safe.
pub type FrameTx = tokio::sync::mpsc::UnboundedSender<Vec<u8>>;

/// Per-viewer sink (daemon -> one viewer). BOUNDED: a viewer that connects then
/// stops reading must not grow the daemon's memory without bound (I3/F4). When
/// this fills we drop the slow viewer instead of blocking the fan-out.
pub type ViewerTx = tokio::sync::mpsc::Sender<Vec<u8>>;

/// Per-viewer bounded channel depth (frames). Each frame is at most one pty read
/// (hub-pty reads in 8 KiB chunks), so the worst-case backlog for a stalled
/// viewer is ~= VIEWER_CHANNEL_BOUND * 8 KiB ≈ 8 MiB before we disconnect it.
/// That is generous enough that a briefly-slow viewer (e.g. the GUI repainting)
/// is never dropped, yet bounds memory hard. A dropped viewer reconnects and
/// gets a fresh Replay (full-screen snapshot), so nothing is lost.
pub const VIEWER_CHANNEL_BOUND: usize = 1024;

/// Grace window a viewer is allowed to be unable to accept a single frame before
/// we conclude its client has genuinely stopped reading and drop it. Fan-out
/// delivers each frame with an *awaiting* send (backpressure), so a viewer that
/// is reading normally always drains a slot well within this window and keeps
/// every frame in order — it is NEVER dropped, even under a multi-MB flood. A
/// viewer whose client stopped reading fills its socket buffer + bounded channel
/// and then cannot accept for the whole window, so it is dropped (bounded
/// memory). The window is long enough to absorb a real repaint/GC pause yet short
/// enough that a truly stalled viewer is evicted promptly.
pub const VIEWER_SEND_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Cumulative-lag window. A viewer whose bounded channel is *continuously* full
/// (it cannot sustain the average output rate) for longer than this is dropped,
/// bounding relay-side memory. Distinct from `VIEWER_SEND_GRACE`, which caps a
/// SINGLE awaiting send: a viewer can accept one frame every few seconds (so it
/// never trips the per-frame timeout) yet still fall permanently behind under a
/// sustained flood — `LAG_GRACE` is what evicts that "slow-alive" viewer. A
/// viewer that keeps up drains a slot promptly, so a `try_send` succeeds and its
/// lag marker is cleared: it is NEVER dropped by this rule.
pub const LAG_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// One attached viewer's fan-out sink. Dropping this struct closes the viewer's
/// socket (its writer task sends a FIN on shutdown), so removing a viewer from
/// the registry also disconnects it — it will reconnect + Replay to resync.
pub struct ViewerSink {
    pub tx: ViewerTx,
    /// Kept solely so its Drop signals the writer task to shut the socket down.
    /// Dropping the whole `ViewerSink` (on removal) fires this.
    pub _kill: tokio::sync::oneshot::Sender<()>,
    /// Cumulative-lag marker: `Some(t)` = the instant this viewer's channel was
    /// first observed Full and has stayed unable to keep up since; `None` = it is
    /// keeping up. Set on the first Full `try_send`, cleared the moment a
    /// `try_send` succeeds again; when `now - t > LAG_GRACE` the viewer is
    /// dropped. Owned by the registry (mutated only under the `Mutex`).
    pub behind_since: Option<Instant>,
}

pub struct Session {
    pub info: SessionInfo,
    pub relay_tx: FrameTx,
    /// viewer_id -> writer sink (fan-out sink for output/closed/replay).
    pub viewers: HashMap<u64, ViewerSink>,
    /// FIFO of viewer_ids awaiting a Replay from the relay (Attach is serialized
    /// through the daemon, so front-of-queue is the correct target).
    pub pending_replay: VecDeque<u64>,
}

#[derive(Default)]
pub struct Inner {
    pub sessions: HashMap<SessionId, Session>,
    pub next_id: u64,
    pub next_viewer: u64,
}

#[derive(Clone, Default)]
pub struct Registry {
    pub inner: Arc<Mutex<Inner>>,
}

impl Registry {
    pub async fn alloc_id(&self) -> SessionId {
        let mut g = self.inner.lock().await;
        g.next_id += 1;
        SessionId(g.next_id)
    }

    /// Ensure future ids exceed any seeded (reconciled) id.
    pub async fn bump_next_id_at_least(&self, floor: u64) {
        let mut g = self.inner.lock().await;
        if g.next_id < floor { g.next_id = floor; }
    }

    pub async fn add_session(&self, info: SessionInfo, relay_tx: FrameTx) {
        let mut g = self.inner.lock().await;
        g.sessions.insert(info.id, Session {
            info, relay_tx, viewers: HashMap::new(), pending_replay: VecDeque::new(),
        });
    }

    pub async fn list(&self) -> Vec<SessionInfo> {
        let g = self.inner.lock().await;
        g.sessions.values().map(|s| s.info.clone()).collect()
    }

    pub async fn remove_session(&self, id: SessionId) {
        let mut g = self.inner.lock().await;
        g.sessions.remove(&id);
    }

    /// Fill in the relay's real pid once its session record appears
    /// (Open registers with `pid: 0`; the relay's record.json has the truth).
    pub async fn set_pid(&self, id: SessionId, pid: u32) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.get_mut(&id) {
            s.info.pid = pid;
        }
    }

    /// Mutate the in-memory `SessionInfo` for `id` with a new shell-integration
    /// activity report (relay -> daemon `ControlMsg::SessionActivity`, design
    /// spec 2026-07-23-shell-integration-design.md §5). This is the SAME map
    /// `Open`/`Opened` populate, so it's immediately visible to the next
    /// `ControlMsg::List` -- the GUI's "healthy" bucket sources `SessionInfo`
    /// from here, not from disk (spec §5's closing paragraph). No-op on an
    /// unknown/already-torn-down session (e.g. a stale message racing a
    /// `Closed`).
    pub async fn update_activity(&self, id: SessionId, cwd: String, last_exit_code: Option<i32>, activity_seq: u64) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.get_mut(&id) {
            s.info.cwd = cwd;
            s.info.last_exit_code = last_exit_code;
            s.info.activity_seq = activity_seq;
        }
    }

    /// Register a viewer sink on a session; returns its viewer_id and queues it
    /// for the next Replay. On an unknown session the sink is handed back
    /// (`Err`) so the caller can report an error before it drops. Also forwards
    /// Attach to the relay so it produces the Replay (and, if this is the first
    /// viewer after a detach, re-starts Output streaming).
    pub async fn attach_viewer(&self, id: SessionId, sink: ViewerSink) -> Result<u64, ViewerSink> {
        let mut g = self.inner.lock().await;
        g.next_viewer += 1;
        let vid = g.next_viewer;
        let s = match g.sessions.get_mut(&id) { Some(s) => s, None => return Err(sink) };
        s.viewers.insert(vid, sink);
        s.pending_replay.push_back(vid);
        let _ = s.relay_tx.send(encode_control(&ControlMsg::Attach { id }));
        Ok(vid)
    }

    pub async fn detach_viewer(&self, id: SessionId, vid: u64) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.get_mut(&id) {
            s.viewers.remove(&vid);
            s.pending_replay.retain(|v| *v != vid);
            // Detach must NOT kill the shell (spec §5). Relay keeps running.
            // But once the LAST viewer leaves, tell the relay to stop streaming
            // Output over its daemon channel (it would otherwise stream forever
            // into a void). It re-starts on the next Attach (which resends a
            // Replay), so this is lossless.
            if s.viewers.is_empty() {
                let _ = s.relay_tx.send(encode_control(&ControlMsg::Detach { id }));
            }
        }
    }

    /// Relay -> daemon output: fan out to every attached viewer of this session.
    ///
    /// CRITICAL (review, load-bearing): we clone the viewer sinks out from under
    /// the registry `Mutex` and release it BEFORE sending, so this never holds the
    /// lock while awaiting a viewer. Cross-SESSION isolation is preserved because
    /// each session's relay has its own `drive_relay` task; a slow viewer here
    /// only paces *this* session's fan-out, never another's.
    ///
    /// Backpressure model (I3/F4). A terminal byte-stream cannot skip bytes for a
    /// viewer we intend to keep (it would desync the screen), so the only lossless
    /// options are "deliver every frame in order" or "drop the viewer entirely".
    /// Two mechanisms, applied per frame:
    ///
    ///   1. Fast pass (inline, non-blocking `try_send` to EVERY viewer first): a
    ///      viewer that is keeping up receives this frame immediately and is never
    ///      delayed behind a slow co-viewer — no head-of-line blocking. It also
    ///      clears the viewer's lag marker, so a fast reader is NEVER dropped.
    ///
    ///   2. Await pass (CONCURRENT, only for viewers whose channel was Full):
    ///      each falls back to an *awaiting* send bounded by `VIEWER_SEND_GRACE`,
    ///      run in parallel so N slow viewers cost ~1 grace, not N. Delivering in
    ///      order keeps a briefly-slow viewer (GUI repaint/GC) without any gap.
    ///
    ///   3. Cumulative-lag drop (bounds relay memory): the FIRST time a viewer's
    ///      channel is Full we stamp `behind_since`; once it has been continuously
    ///      behind for `LAG_GRACE` we drop it. This is what evicts a "slow-alive"
    ///      viewer that accepts a frame every few seconds (never tripping the
    ///      per-frame timeout) yet cannot sustain the average rate — otherwise it
    ///      would pace the whole `drive_relay` loop to its rate and the relay would
    ///      buffer the shell's output unbounded. A `Closed` channel is dropped at
    ///      once. A dropped viewer reconnects and gets a fresh Replay (lossless).
    ///
    /// The shell never stalls: the relay buffers pty output on its own side, so
    /// pausing this fan-out only grows the relay's buffer transiently — and the
    /// lag drop bounds even that by evicting a persistently-behind viewer.
    pub async fn fan_out_output(&self, id: SessionId, bytes: &[u8]) {
        // 1. Snapshot each viewer's sink + current lag marker under the lock, then
        // release it. Never hold the registry lock across a send/.await.
        let sinks: Vec<(u64, ViewerTx, Option<Instant>)> = {
            let g = self.inner.lock().await;
            match g.sessions.get(&id) {
                Some(s) => s.viewers.iter()
                    .map(|(vid, sink)| (*vid, sink.tx.clone(), sink.behind_since))
                    .collect(),
                None => return,
            }
        };
        if sinks.is_empty() { return; }

        let frame = encode_data(id, bytes);
        let now = Instant::now();

        // 2a. Fast pass: non-blocking try_send to every viewer FIRST.
        let mut evict: Vec<u64> = Vec::new();
        let mut updates: Vec<(u64, Option<Instant>)> = Vec::new(); // only CHANGED lag markers
        let mut deferred: Vec<(u64, ViewerTx, Vec<u8>)> = Vec::new();
        use tokio::sync::mpsc::error::TrySendError;
        for (vid, tx, prev) in sinks {
            match tx.try_send(frame.clone()) {
                Ok(()) => { if prev.is_some() { updates.push((vid, None)); } } // caught up
                Err(TrySendError::Closed(_)) => evict.push(vid),               // socket gone
                Err(TrySendError::Full(f)) => {
                    let since = prev.unwrap_or(now);
                    if now.duration_since(since) > LAG_GRACE {
                        evict.push(vid); // persistently behind -> drop (reconnects + Replay)
                    } else {
                        if prev.is_none() { updates.push((vid, Some(since))); } // first fell behind
                        deferred.push((vid, tx, f)); // deliver this frame in order below
                    }
                }
            }
        }

        // 2b. Await pass (concurrent): only viewers whose channel was Full. The
        // fast viewers above already have this frame, so they are not blocked
        // behind these. A viewer that accepts within grace keeps its frame in
        // order; one that cannot (socket wedged) is evicted.
        if !deferred.is_empty() {
            let mut set = tokio::task::JoinSet::new();
            for (vid, tx, f) in deferred {
                set.spawn(async move {
                    match tokio::time::timeout(VIEWER_SEND_GRACE, tx.send(f)).await {
                        Ok(Ok(())) => (vid, true),  // delivered in order (still behind)
                        _ => (vid, false),          // closed or stalled a full frame -> evict
                    }
                });
            }
            while let Some(res) = set.join_next().await {
                if let Ok((vid, ok)) = res { if !ok { evict.push(vid); } }
            }
        }

        if evict.is_empty() && updates.is_empty() { return; }

        // 3. Re-acquire the lock only to persist lag markers + evict dead viewers.
        // Dropping each ViewerSink closes that viewer's socket (it reconnects +
        // Replay). Viewers may have detached meanwhile, so guard every lookup.
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.get_mut(&id) {
            for (vid, mark) in updates {
                if let Some(sink) = s.viewers.get_mut(&vid) { sink.behind_since = mark; }
            }
            let evicted_any = !evict.is_empty();
            for vid in evict {
                s.viewers.remove(&vid);
                s.pending_replay.retain(|v| *v != vid);
            }
            if evicted_any && s.viewers.is_empty() {
                let _ = s.relay_tx.send(encode_control(&ControlMsg::Detach { id }));
            }
        }
    }

    /// Relay -> daemon Replay: deliver only to the front-of-FIFO viewer.
    pub async fn deliver_replay(&self, id: SessionId, screen: Vec<u8>) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.get_mut(&id) {
            if let Some(vid) = s.pending_replay.pop_front() {
                let replay = encode_control(&ControlMsg::Replay { id, screen });
                // A viewer is inserted into `viewers` at attach time, so `fan_out_output`
                // can push Output frames to it BEFORE its Replay arrives. Under a flood its
                // bounded channel can already be FULL when the Replay lands here — a
                // non-blocking `try_send` then FAILS. We must NOT keep such a viewer: it
                // would render Output deltas against a blank screen forever (permanent
                // desync). Instead EVICT it (remove from `viewers` + `pending_replay`);
                // dropping its `ViewerSink` closes the socket, the client reconnects and
                // gets a fresh Replay -> lossless resync. Same for a Closed channel.
                let drop_it = match s.viewers.get(&vid) {
                    Some(sink) => sink.tx.try_send(replay).is_err(),
                    None => false, // viewer already detached; nothing to deliver or evict
                };
                if drop_it {
                    s.viewers.remove(&vid);
                    s.pending_replay.retain(|v| *v != vid);
                    // Mirror the other eviction sites: if that was the last viewer, tell
                    // the relay to stop streaming Output into a void.
                    if s.viewers.is_empty() {
                        let _ = s.relay_tx.send(encode_control(&ControlMsg::Detach { id }));
                    }
                }
            }
        }
    }

    /// Viewer -> relay input.
    pub async fn route_input(&self, id: SessionId, bytes: Vec<u8>) {
        let g = self.inner.lock().await;
        if let Some(s) = g.sessions.get(&id) { let _ = s.relay_tx.send(encode_data(id, &bytes)); }
    }

    /// Viewer -> relay control (Resize/ClaimSize/Detach/Kill).
    pub async fn route_control_to_relay(&self, id: SessionId, msg: &ControlMsg) {
        let g = self.inner.lock().await;
        if let Some(s) = g.sessions.get(&id) { let _ = s.relay_tx.send(encode_control(msg)); }
    }

    /// Relay -> daemon Closed: notify all viewers, then drop the session.
    pub async fn broadcast_closed(&self, id: SessionId, exit_code: Option<i32>) {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.sessions.remove(&id) {
            let frame = encode_control(&ControlMsg::Closed { id, exit_code });
            for sink in s.viewers.values() { let _ = sink.tx.try_send(frame.clone()); }
        }
    }
}
