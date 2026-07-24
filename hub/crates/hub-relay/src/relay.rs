//! The per-session relay actor. Owns the pty and the headless Screen; serves
//! daemon channels (Task 6/7); applies debounced resizes (Task 8); tears down
//! by origin (Task 9). This task establishes pty ownership + the event loop's
//! output/exit sources.

use hub_proto::{encode_data, Frame, Origin};
use hub_pty::{Pty, PtyOutput, PtySize};
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use hub_term::Screen;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone, Debug)]
pub struct RelayConfig {
    pub shell: String,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
    pub term: String,
    pub origin: Origin,
    pub title: String,
}

/// Everything that can wake the relay actor.
#[derive(Debug)]
pub enum RelayEvent {
    /// Raw pty output bytes.
    Output(Vec<u8>),
    /// Child exited (exit-code-only per Plan 1; None if unknown/signal).
    Exit(Option<i32>),
    /// A daemon channel connected: (chan_id, its writer sink).
    ChannelUp(u64, UnboundedSender<Vec<u8>>),
    /// A daemon channel disconnected.
    ChannelDown(u64),
    /// A decoded frame arrived on a daemon channel.
    Frame(u64, Frame),
    /// Bytes from the External primary (outer terminal on fd 0).
    PrimaryInput(Vec<u8>),
    /// Outer terminal closed (fd 0 EOF) — External teardown trigger.
    PrimaryClosed,
    /// Debounced, changed size to apply to the pty.
    ApplyResize(u16, u16),
}

pub struct RelayState;

/// The environment a relay injects into the shell it spawns: `cfg.env` plus a
/// mandatory `HUB_ACTIVE=1` and `TERM=<cfg.term>`.
///
/// A relay-spawned shell is BY DEFINITION a hub-managed shell, so it must mark
/// itself active. The shell-rc integration snippet guards with
/// `[ -z "$HUB_ACTIVE" ]` (the re-exec guard), so with `HUB_ACTIVE=1` the shell
/// does NOT run `hub attach --new` again on startup.
///
/// WHY THE RELAY MUST DO THIS (not just the CLI): the `hub attach` path
/// (`hub-cli::attach`) already exec's the relay with `HUB_ACTIVE=1` in its own
/// env, so an External-origin shell inherits it. But a Hub-origin session is
/// spawned by the GUI as `hub-relay --origin hub --detach` DIRECTLY — the relay
/// process has no `HUB_ACTIVE` to inherit, so without this its shell sources
/// `~/.zshrc`, the hook re-fires, and it spawns a NESTED External relay whose
/// pty is bridged back onto this shell's pty. The two sessions then mirror each
/// other (the "External tile mirrors the Hub session" bug). Injecting here — the
/// single chokepoint where every relay shell is spawned — fixes every origin;
/// it's idempotent/harmless for the External path that already had it.
///
/// `TERM` needs the exact same treatment and for the exact same reason: without
/// it, `portable_pty::CommandBuilder` doesn't clear the environment before
/// spawning, so the child shell falls back to whatever ambient `TERM` the
/// `hub-relay` PROCESS itself happened to inherit. For External-origin (spawned
/// from inside a real terminal via `hub attach`) that's a harmless accident --
/// it inherits Terminal.app/iTerm's own correct `TERM`. For Hub-origin (spawned
/// directly by the GUI app, which has no controlling terminal) it's unset or
/// stale, so zsh falls back to a terminfo entry that doesn't match what xterm.js
/// actually emulates. The shell's line editor (ZLE) and any prompt
/// theme/plugin issuing capability-dependent cursor-movement escapes then
/// desyncs from xterm.js's interpretation of them -- visible as corrupted,
/// overlapping redraws on every keystroke, persisting for the life of the
/// session (this is a fixed env-var mismatch, not a timing race, so it doesn't
/// self-correct or depend on when the user starts typing). `cfg.term` is
/// already parsed and defaults to `"xterm-256color"` (see `main.rs`'s `--term`),
/// exactly matching xterm.js's `@xterm/xterm` capability set -- it just never
/// made it into the child's actual environment before this fix.
pub fn shell_env(cfg: &RelayConfig) -> Vec<(String, String)> {
    let mut env = cfg.env.clone();
    // Pushed LAST so these always win over any (currently none) cfg.env entry.
    env.push(("HUB_ACTIVE".to_string(), "1".to_string()));
    env.push(("TERM".to_string(), cfg.term.clone()));
    env
}

impl RelayState {
    /// Spawn the inner pty + shell and a matching headless Screen.
    pub fn spawn_pty(cfg: &RelayConfig) -> anyhow::Result<(Pty, PtyOutput, Screen)> {
        // R1: the initial size, unlike every later resize, never passed through
        // `clamp_resize`'s min-1 floor -- `--cols 0 --rows 0` at spawn would hand
        // portable-pty's `openpty`/`vt100::Parser::new` a degenerate 0x0 size.
        // Clamp here the same way `ApplyResize` clamps subsequent resizes.
        let cols = cfg.cols.max(1);
        let rows = cfg.rows.max(1);
        let (pty, out) = Pty::spawn(
            &cfg.shell, &cfg.cwd, &shell_env(cfg),
            PtySize { cols, rows },
        )?;
        // Screen default scrollback 10k (spec §10). Note: replay = visible grid only.
        let screen = Screen::new(rows, cols, 10_000);
        Ok((pty, out, screen))
    }
}

/// Bridge blocking pty reads + the one-shot exit into async `RelayEvent`s.
/// Uses std threads (blocking recv); tokio UnboundedSender::send is sync-safe.
pub fn bridge_pty(out: PtyOutput, ev_tx: UnboundedSender<RelayEvent>) {
    let tx_out = ev_tx.clone();
    std::thread::spawn(move || {
        while let Ok(bytes) = out.rx.recv() {
            if tx_out.send(RelayEvent::Output(bytes)).is_err() { break; }
        }
    });
    std::thread::spawn(move || {
        let code = out.exit_rx.recv().ok().flatten();
        let _ = ev_tx.send(RelayEvent::Exit(code));
    });
}

use crate::conn::{write_frame, FrameReader};
use crate::paths::HubPaths;
use crate::record::SessionRecord;
use hub_proto::{encode_control, ControlMsg, SessionId};
use tokio::sync::mpsc;

/// Puts the OUTER terminal (fd 0) into raw mode for the lifetime of an
/// External-origin relay, restoring the original settings on drop.
///
/// WHY: an External relay owns the user's real terminal (fds 0/1) and bridges it
/// to the INNER pty (`spawn_pty`), whose shell does its own canonical-mode line
/// editing + echo. If the outer terminal is left in its default cooked mode, it
/// ALSO line-buffers and echoes — so keystrokes are echoed twice, input isn't
/// delivered until Enter, and control sequences show up literally: arrow keys
/// render as `^[[A^[[B^[[C^[[D` instead of driving the inner shell's ZLE. The
/// outer terminal must be raw (like ssh/tmux/script do) so bytes pass through
/// verbatim; the inner pty is the only one doing cooked mode + echo.
///
/// Restored on Drop, which covers every normal exit path (inner shell exits ->
/// actor loop ends -> `run_relay` returns; any `?`-return earlier). A hard
/// SIGKILL can't restore, but in that case the outer terminal is being torn down
/// anyway.
struct RawTermGuard {
    fd: std::os::fd::RawFd,
    orig: libc::termios,
}
impl RawTermGuard {
    /// The relay's stdin (fd 0) IS the outer terminal on the own_stdio path.
    fn install() -> Option<RawTermGuard> {
        Self::install_fd(0)
    }
    /// No-op (`None`) when `fd` isn't a real terminal (piped/redirected) or a
    /// termios call fails — never mangles a non-tty. Split from `install` so it
    /// can be tested against a real openpty fd instead of the process's fd 0.
    fn install_fd(fd: std::os::fd::RawFd) -> Option<RawTermGuard> {
        unsafe {
            if libc::isatty(fd) != 1 {
                return None;
            }
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut orig) != 0 {
                return None;
            }
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawTermGuard { fd, orig })
        }
    }
}
impl Drop for RawTermGuard {
    fn drop(&mut self) {
        // Best-effort restore of the saved (cooked) settings.
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.orig);
        }
    }
}

fn write_raw(fd: std::os::fd::RawFd, bytes: &[u8]) -> std::io::Result<()> {
    let n = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
    if n == bytes.len() as isize { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// Enables DECSET 1004 "focus reporting" on the OUTER terminal for the
/// lifetime of an External-origin relay, disabling it again on drop.
///
/// WHY: this is how the relay learns the user switched attention BACK to this
/// vendor terminal (as opposed to e.g. a Hub GUI tile), so it can re-claim the
/// pty's real size (see `strip_focus_reports`) instead of staying pinned
/// wherever the last viewer left it. Terminals that don't support the mode
/// (plain Terminal.app, a piped/redirected fd) just never send the report —
/// silent no-op, sizing falls back to last-claim-wins (spec §7's documented
/// fallback).
struct FocusReportGuard {
    fd: std::os::fd::RawFd,
}
impl FocusReportGuard {
    /// The relay's stdout (fd 1) IS the outer terminal on the own_stdio path.
    fn install() -> Option<FocusReportGuard> {
        Self::install_fd(1)
    }
    /// No-op (`None`) when `fd` isn't a real terminal — never writes an
    /// escape sequence into a pipe/file. Split from `install` so it can be
    /// tested against a real openpty fd instead of the process's fd 1.
    fn install_fd(fd: std::os::fd::RawFd) -> Option<FocusReportGuard> {
        unsafe {
            if libc::isatty(fd) != 1 {
                return None;
            }
        }
        write_raw(fd, b"\x1b[?1004h").ok()?;
        Some(FocusReportGuard { fd })
    }
}
impl Drop for FocusReportGuard {
    fn drop(&mut self) {
        let _ = write_raw(self.fd, b"\x1b[?1004l");
    }
}

/// Entry point for a real relay run. Implemented incrementally: pty (Task 3),
/// register + record + serve (Task 6/7), resize (Task 8), teardown (Task 9).
///
/// `own_stdio`: true only when this relay owns the process's real stdin/stdout
/// (i.e. it is the standalone `hub-relay` binary launched by an outer terminal).
/// The External primary bridge reads the process-global `tokio::io::stdin()`, so
/// it MUST NOT be spawned when `run_relay` is embedded in another process (tests,
/// or a daemon that drives I/O over sockets) — otherwise that host process's
/// unrelated stdin EOF would be mistaken for the outer terminal closing and would
/// immediately tear the session down.
pub async fn run_relay(cfg: RelayConfig, daemon_sock: Option<String>, own_stdio: bool) -> anyhow::Result<()> {
    let paths = HubPaths::from_env();
    let dsock = daemon_sock
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths.daemon_sock());

    // 1. Spawn pty + screen.
    let (pty, out, screen) = RelayState::spawn_pty(&cfg)?;

    // F1 auth: obtain the per-install token. `ensure_token` reads the
    // daemon-created token (or creates one for a directly-spawned relay on a
    // fresh HUB_DIR). We authenticate to the daemon with it AND require it from
    // anyone (the daemon) dialing our per-session socket.
    let token = hub_transport::auth::ensure_token(paths.base())?;

    // 2. Dial the daemon, send Hello (auth), then Open, await Opened{id}.
    let stream = tokio::net::UnixStream::connect(&dsock).await?;
    let (rd, mut wr) = stream.into_split();
    let mut fr = FrameReader::new(rd);
    hub_transport::auth::send_hello(&mut wr, &token).await?; // MUST be first frame
    // F5: env is deliberately NOT sent to the daemon — the daemon owns no pty
    // and never reads it, so putting the shell's environment (which can carry
    // secrets/tokens) on the wire would be pure exposure with no benefit. The
    // pty itself already got `cfg.env` directly via `RelayState::spawn_pty`
    // above (step 1), sourced from this process's own env, not from any
    // message — dropping it here changes no runtime behavior.
    let open = ControlMsg::Open {
        shell: cfg.shell.clone(), cwd: cfg.cwd.clone(),
        cols: cfg.cols, rows: cfg.rows, term: cfg.term.clone(),
        origin: cfg.origin, title: cfg.title.clone(),
    };
    write_frame(&mut wr, &encode_control(&open)).await?;
    let id = loop {
        match fr.next().await? {
            Some(hub_proto::Frame::Control(ControlMsg::Opened { id })) => break id,
            Some(_) => continue,
            None => anyhow::bail!("daemon closed before Opened"),
        }
    };
    tracing::info!("relay registered as session {}", id.0);

    // 3. Bind our own per-session socket (for probes + daemon reconnects).
    // Defensive: the daemon normally already created ~/.hub/sessions when we
    // dialed it above, but a directly-spawned relay against a fresh HUB_DIR
    // must not fail here. Idempotent — safe even when the dir already exists.
    paths.ensure_dirs()?;
    let sock_path = paths.sock(id);
    let _ = std::fs::remove_file(&sock_path);
    let session_listener = tokio::net::UnixListener::bind(&sock_path)?;
    // F3: this is a raw UnixListener::bind, NOT hub_transport::bind_listener
    // (which already chmods 0600 for the daemon socket) — so the per-session
    // socket needs its own explicit chmod. Until this line, the file exists
    // with the umask-default mode and is protected only by the 0700 sessions/
    // dir; make the socket itself 0600 too (defense in depth).
    std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600))?;

    // 4. Write the record atomically (temp+rename).
    let rec = SessionRecord {
        record_version: 1, id, origin: cfg.origin, title: cfg.title.clone(),
        pid: std::process::id(), started_unix: now_unix(),
        cols: cfg.cols, rows: cfg.rows,
        sock: sock_path.to_string_lossy().into(),
        // Shell integration (spec §5): empty/unknown until the actor's
        // ShellIntegration scanner sees the shell's first OSC 7/133 event.
        cwd: String::new(), last_exit_code: None, activity_seq: 0,
    };
    rec.write_atomic(&paths)?;

    // 5. Run the actor over the initial channel + the session socket.
    let ev = build_event_bus();
    bridge_pty(out, ev.tx.clone());
    serve_initial_channel(fr_into_reader(fr), wr, ev.tx.clone(), id);
    accept_session_socket(session_listener, ev.tx.clone(), id, token.clone());
    // Built before the primary bridge / SIGWINCH watcher so both can push
    // through the same debounce path that Attach/ClaimSize/Resize use.
    let resize_in = crate::resize::spawn_coalescer(cfg.cols, cfg.rows, ev.tx.clone());
    // The outer terminal must be raw while we bridge it to the inner pty, or
    // arrow keys echo as `^[[A` and input is line-buffered/double-echoed. Held
    // for the whole run; Drop restores the terminal when `run_relay` returns.
    // Only for the External own_stdio path (a Hub-origin relay owns no outer
    // terminal, and an embedded relay must not touch the host's stdin).
    let _raw_guard = if own_stdio && matches!(cfg.origin, Origin::External) {
        RawTermGuard::install()
    } else {
        None
    };
    // Lets the outer terminal report focus in/out (see `FocusReportGuard`),
    // so `strip_focus_reports` can re-claim the pty's real size when the user
    // switches attention back to it. Held for the same lifetime as raw mode.
    let _focus_guard = if own_stdio && matches!(cfg.origin, Origin::External) {
        FocusReportGuard::install()
    } else {
        None
    };
    if own_stdio && matches!(cfg.origin, Origin::External) {
        spawn_primary_bridge(ev.tx.clone(), resize_in.clone()); // defined in Task 9
        // External-origin only: the outer terminal is OUR controlling
        // terminal (fds 0/1). Hub-origin has no outer terminal to resize, so
        // it's gated identically to the primary bridge above.
        spawn_sigwinch_watcher(resize_in.clone());
    }
    let mut actor = RelayActor::new(cfg, id, paths, pty, screen);
    actor.resize_in = Some(resize_in);
    // I4: External origin owns the outer terminal's stdout — route it through a
    // dedicated task so a Ctrl-S'd terminal can't stall the actor. Hub origin
    // has no outer terminal, so it stays `None`.
    if matches!(actor.cfg.origin, Origin::External) {
        actor.primary_sink = Some(spawn_primary_stdout());
    }
    run_actor(actor, ev).await
}

fn fr_into_reader(fr: FrameReader<tokio::net::unix::OwnedReadHalf>) -> FrameReader<tokio::net::unix::OwnedReadHalf> { fr }

/// External-origin primary bridge: outer terminal is fd 0 (stdin) / fd 1
/// (stdout, written from the actor's `stdout`). stdin EOF => PrimaryClosed.
///
/// Bytes are passed through `strip_focus_reports` first: `FocusReportGuard`
/// asks the outer terminal to send DECSET 1004 focus events, and this is
/// where the relay consumes them (see that function for why).
fn spawn_primary_bridge(ev: mpsc::UnboundedSender<RelayEvent>, resize_in: mpsc::UnboundedSender<(u16, u16)>) {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut stdin = tokio::io::stdin();
        let mut buf = vec![0u8; 32 * 1024];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) => { let _ = ev.send(RelayEvent::PrimaryClosed); break; }
                Ok(n) => {
                    let forward = strip_focus_reports(&buf[..n], libc::STDOUT_FILENO, &resize_in);
                    if ev.send(RelayEvent::PrimaryInput(forward)).is_err() { break; }
                }
                Err(_) => { let _ = ev.send(RelayEvent::PrimaryClosed); break; }
            }
        }
    });
}

/// Removes DECSET 1004 focus-report sequences (`ESC [ I` focus-in / `ESC [ O`
/// focus-out) from bytes arriving on the outer terminal before they're
/// forwarded to the inner pty — the inner shell never asked for these,
/// only `FocusReportGuard` did, on the relay's own behalf, so they must not
/// leak through as literal keystrokes.
///
/// On focus-IN, re-reads the outer terminal's real size off `term_fd` and
/// pushes it into `resize_in` — the same debounced path SIGWINCH/ClaimSize/
/// Resize use (spec §7 "Focus-follows-size"). This is what lets moving focus
/// BACK to the vendor terminal snap the pty back to its own real size,
/// instead of staying pinned wherever a Hub tile's `ClaimSize` last left it.
pub fn strip_focus_reports(data: &[u8], term_fd: i32, resize_in: &mpsc::UnboundedSender<(u16, u16)>) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x1b && i + 2 < data.len() && data[i + 1] == b'[' && matches!(data[i + 2], b'I' | b'O') {
            if data[i + 2] == b'I' {
                if let Some((cols, rows)) = read_term_size(term_fd) {
                    let _ = resize_in.send((cols, rows));
                }
            }
            i += 3;
            continue;
        }
        out.push(data[i]);
        i += 1;
    }
    out
}

/// Read the current terminal size from `fd` via TIOCGWINSZ. Returns `None` on
/// error or a degenerate (0-width) result.
pub fn read_term_size(fd: i32) -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 && ws.ws_row > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

/// External-origin only: the relay's own controlling terminal is the outer
/// terminal (fds 0/1). When the user resizes THAT terminal, the kernel sends
/// this process SIGWINCH — nothing else would notice, so the inner pty/shell
/// would stay stuck at its spawn size (stale full-screen apps like vim/htop).
/// On each SIGWINCH, re-read the real size off fd 1 and push it through the
/// same debounce/apply path Attach/ClaimSize/Resize already use, so pty +
/// Screen both update consistently.
fn spawn_sigwinch_watcher(resize_in: mpsc::UnboundedSender<(u16, u16)>) {
    tokio::spawn(async move {
        let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change()) {
            Ok(s) => s,
            Err(e) => { tracing::warn!("SIGWINCH watcher unavailable: {e:#}"); return; }
        };
        while sig.recv().await.is_some() {
            if let Some((cols, rows)) = read_term_size(libc::STDOUT_FILENO) {
                let _ = resize_in.send((cols, rows));
            }
        }
    });
}

/// Clean shell teardown: SIGHUP the child (NOT SIGKILL). Plan 1's Pty::kill is
/// SIGKILL, so we signal directly. The child exit then flows via exit_rx ->
/// RelayEvent::Exit -> Closed + record/socket cleanup.
fn sighup_shell(a: &mut RelayActor) {
    if let Some(pid) = a.pty.child_pid() {
        tracing::info!("SIGHUP session {} child pid {}", a.id.0, pid);
        unsafe { libc::kill(pid as i32, libc::SIGHUP); }
    }
}

/// Clamp a resize target so pty/Screen never see a 0x0 dimension.
pub fn clamp_resize(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(1), rows.max(1))
}

fn now_unix() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs()).unwrap_or(0)
}

pub struct EventBus {
    pub tx: mpsc::UnboundedSender<RelayEvent>,
    pub rx: mpsc::UnboundedReceiver<RelayEvent>,
}
pub fn build_event_bus() -> EventBus {
    let (tx, rx) = mpsc::unbounded_channel();
    EventBus { tx, rx }
}

/// One connection between the relay and a daemon (initial dial OR an inbound
/// accept on <id>.sock). Reader task -> RelayEvent::Frame; writer sink stored
/// by the actor to push Output/Replay/Closed.
fn spawn_channel<R>(chan_id: u64, mut fr: FrameReader<R>, wr_tx: mpsc::UnboundedSender<Vec<u8>>, ev: mpsc::UnboundedSender<RelayEvent>)
where R: tokio::io::AsyncRead + Unpin + Send + 'static {
    let _ = ev.send(RelayEvent::ChannelUp(chan_id, wr_tx));
    tokio::spawn(async move {
        loop {
            match fr.next().await {
                Ok(Some(frame)) => { if ev.send(RelayEvent::Frame(chan_id, frame)).is_err() { break; } }
                _ => break,
            }
        }
        let _ = ev.send(RelayEvent::ChannelDown(chan_id));
    });
}

/// I4: dedicated External-primary stdout writer. The actor pushes output bytes
/// here via `try_send` (non-blocking); THIS task does the blocking `write_all`.
/// A flow-controlled (Ctrl-S'd) outer terminal blocks only this task — never the
/// actor loop — so viewers/input/resize keep flowing. The channel is BOUNDED; on
/// full we drop the frame at the call site (the primary is the user's own paused
/// terminal, and the headless Screen still has full state for viewers/Replay),
/// which keeps memory bounded without ever blocking the actor.
const PRIMARY_STDOUT_BOUND: usize = 1024;
fn spawn_primary_stdout() -> mpsc::Sender<Vec<u8>> {
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(PRIMARY_STDOUT_BOUND);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut so = tokio::io::stdout();
        while let Some(bytes) = rx.recv().await {
            if so.write_all(&bytes).await.is_err() { break; }
            let _ = so.flush().await;
        }
    });
    tx
}

fn writer_sink(mut wr: impl tokio::io::AsyncWrite + Unpin + Send + 'static) -> mpsc::UnboundedSender<Vec<u8>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        while let Some(bytes) = rx.recv().await {
            if crate::conn::write_frame(&mut wr, &bytes).await.is_err() { break; }
        }
    });
    tx
}

/// Serve the initial daemon dial connection (already past Open/Opened).
fn serve_initial_channel(fr: FrameReader<tokio::net::unix::OwnedReadHalf>, wr: tokio::net::unix::OwnedWriteHalf, ev: mpsc::UnboundedSender<RelayEvent>, _id: SessionId) {
    let sink = writer_sink(wr);
    spawn_channel(0, fr, sink, ev);
}

/// Accept further daemon connections + reconciliation probes on <id>.sock.
///
/// F1: every connection here (the daemon's reverse-proxy dial, re-adoption
/// after a daemon restart, or a reconcile liveness probe) MUST authenticate.
/// We check the peer uid, then require a valid `Hello { token }` first frame.
/// The verify (with its bounded timeout) runs in a per-connection task so a
/// silent/hostile peer can never stall the accept loop, and a rejected
/// connection is simply dropped (logged without the token).
fn accept_session_socket(listener: tokio::net::UnixListener, ev: mpsc::UnboundedSender<RelayEvent>, _id: SessionId, token: String) {
    use std::os::unix::io::AsRawFd;
    tokio::spawn(async move {
        let mut next_chan = 1u64;
        loop {
            let (stream, _) = match listener.accept().await { Ok(x) => x, Err(_) => break };
            let chan = next_chan; next_chan += 1;
            let ev = ev.clone();
            let token = token.clone();
            tokio::spawn(async move {
                if !hub_transport::auth::peer_uid_ok(stream.as_raw_fd()) {
                    tracing::warn!("rejected <id>.sock connection: peer uid mismatch");
                    return;
                }
                let (rd, wr) = stream.into_split();
                let mut fr = FrameReader::new(rd);
                if let Err(e) = fr.verify_hello(&token).await {
                    tracing::warn!("rejected <id>.sock connection: {e}");
                    return;
                }
                let sink = writer_sink(wr);
                spawn_channel(chan, fr, sink, ev);
            });
        }
    });
}

pub struct RelayActor {
    pub cfg: RelayConfig,
    pub id: SessionId,
    pub paths: HubPaths,
    pub pty: Pty,
    pub screen: Screen,
    pub info: hub_proto::SessionInfo,
    channels: HashMap<u64, mpsc::UnboundedSender<Vec<u8>>>,
    attached: std::collections::HashSet<u64>,
    /// External-primary stdout sink (I4). `None` for Hub origin (no outer
    /// terminal). Set by `run_relay` for External. The actor `try_send`s output
    /// here (non-blocking); a dedicated task does the blocking write, so a
    /// Ctrl-S'd outer terminal can never freeze the actor loop.
    pub primary_sink: Option<mpsc::Sender<Vec<u8>>>,
    pub cur_cols: u16,
    pub cur_rows: u16,
    pub resize_in: Option<mpsc::UnboundedSender<(u16, u16)>>,
    /// Shell-integration OSC 7/133 scanner (design spec §5/§7). Fed the exact
    /// same bytes as `screen.feed` in the `RelayEvent::Output` handler -- a
    /// lightweight sibling tokenize-only pass, never a second full grid.
    shell_integration: hub_term::ShellIntegration,
}

impl RelayActor {
    pub fn new(cfg: RelayConfig, id: SessionId, paths: HubPaths, pty: Pty, screen: Screen) -> Self {
        let info = hub_proto::SessionInfo {
            id, origin: cfg.origin, title: cfg.title.clone(), pid: std::process::id(),
            started_unix: now_unix(), cols: cfg.cols, rows: cfg.rows,
            // Shell integration (spec §5): empty/unknown until the scanner
            // sees the shell's first OSC 7/133 event.
            cwd: String::new(), last_exit_code: None, activity_seq: 0,
        };
        let (cc, cr) = (cfg.cols, cfg.rows);
        Self { cfg, id, paths, pty, screen, info, channels: HashMap::new(),
               attached: Default::default(), primary_sink: None, cur_cols: cc, cur_rows: cr, resize_in: None,
               shell_integration: hub_term::ShellIntegration::new() }
    }

    fn send_to_attached(&self, frame: &[u8]) {
        for cid in &self.attached {
            if let Some(tx) = self.channels.get(cid) { let _ = tx.send(frame.to_vec()); }
        }
    }
}

/// Rewrite this relay's on-disk `SessionRecord` after a shell-integration
/// event (design spec §5): atomic write, same `write_atomic` pattern used at
/// registration, so a crashed relay's ghost record still shows its
/// last-known cwd/exit code. Best-effort -- a write failure is logged, not
/// fatal to the actor loop.
fn write_activity_record(a: &RelayActor) {
    let rec = SessionRecord {
        record_version: 1,
        id: a.id,
        origin: a.cfg.origin,
        title: a.cfg.title.clone(),
        pid: a.info.pid,
        started_unix: a.info.started_unix,
        cols: a.cur_cols,
        rows: a.cur_rows,
        sock: a.paths.sock(a.id).to_string_lossy().into(),
        cwd: a.info.cwd.clone(),
        last_exit_code: a.info.last_exit_code,
        activity_seq: a.info.activity_seq,
    };
    if let Err(e) = rec.write_atomic(&a.paths) {
        tracing::warn!("session {}: failed to rewrite record after shell-integration event: {e:#}", a.id.0);
    }
}

/// Push the new activity to the daemon over the relay's existing persistent
/// control connection (design spec §5) -- no new connection/socket. Sent to
/// every channel currently registered (mirrors how `RelayEvent::Exit`'s
/// `Closed` is broadcast above): the daemon's connection to this relay is
/// always one of `a.channels`, whichever chan_id it currently holds (it need
/// not be `attached`, since attachment only gates VIEWER output streaming --
/// this is a direct relay->daemon control push).
fn send_activity_to_daemon(a: &RelayActor) {
    let msg = encode_control(&ControlMsg::SessionActivity {
        id: a.id,
        cwd: a.info.cwd.clone(),
        last_exit_code: a.info.last_exit_code,
        activity_seq: a.info.activity_seq,
    });
    for tx in a.channels.values() { let _ = tx.send(msg.clone()); }
}

pub async fn run_actor(mut a: RelayActor, mut ev: EventBus) -> anyhow::Result<()> {
    while let Some(event) = ev.rx.recv().await {
        match event {
            RelayEvent::Output(bytes) => {
                a.screen.feed(&bytes);
                // Shell integration (design spec §5/§7): a lightweight sibling
                // tokenize-only pass over the SAME bytes just fed to `screen`
                // above -- purely event-driven off this existing byte stream,
                // no new polling loop. NOT stripped from what's forwarded to
                // viewers below (unlike `strip_focus_reports`, which strips
                // stdin focus-report bytes on the INPUT side) -- these OSC
                // sequences are already invisible/no-op to `vt100` and to
                // xterm.js on the frontend.
                for shell_ev in a.shell_integration.feed(&bytes) {
                    match shell_ev {
                        hub_term::ShellEvent::Cwd(cwd) => { a.info.cwd = cwd; }
                        hub_term::ShellEvent::CommandFinished(code) => {
                            a.info.last_exit_code = Some(code);
                            a.info.activity_seq += 1;
                        }
                    }
                    write_activity_record(&a);
                    send_activity_to_daemon(&a);
                }
                // I4: hand the primary's stdout to its dedicated task via a
                // non-blocking try_send. A Ctrl-S'd outer terminal blocks only
                // THAT task; the actor keeps serving viewers/input/resize.
                //
                // DELIBERATE LOSS ON FULL: a flow-controlled (Ctrl-S'd) outer
                // terminal stops draining, so its dedicated writer task stalls and
                // the bounded primary channel fills (PRIMARY_STDOUT_BOUND * up to
                // one pty read ≈ 8 MiB ceiling). Once full we DROP this frame for
                // the primary rather than block or grow unbounded — this protects
                // the viewers and the actor loop from a paused primary. Consequence:
                // the outer terminal loses those primary-stdout bytes and may show
                // stale content until the next full repaint (e.g. a TUI redraw, or
                // the SIGWINCH nudge on the next attach/resize). The headless Screen
                // still has full state, so viewers/Replay are unaffected.
                if let Some(ps) = a.primary_sink.as_ref() { let _ = ps.try_send(bytes.clone()); }
                let frame = encode_data(a.id, &bytes);
                a.send_to_attached(&frame);
            }
            RelayEvent::Exit(code) => {
                let closed = encode_control(&ControlMsg::Closed { id: a.id, exit_code: code });
                for tx in a.channels.values() { let _ = tx.send(closed.clone()); }
                break;
            }
            RelayEvent::ChannelUp(cid, sink) => { a.channels.insert(cid, sink); }
            RelayEvent::ChannelDown(cid) => { a.channels.remove(&cid); a.attached.remove(&cid); }
            RelayEvent::Frame(cid, frame) => handle_channel_frame(&mut a, cid, frame),
            RelayEvent::PrimaryInput(bytes) => { let _ = a.pty.write(&bytes); }
            RelayEvent::PrimaryClosed => {
                if matches!(a.cfg.origin, Origin::External) { sighup_shell(&mut a); }
                // Hub-origin has no primary bridge, so this never fires for Hub.
            }
            RelayEvent::ApplyResize(cols, rows) => {
                // Guard against 0x0 (a race between coalescer/client and a
                // teardown, or a plain bad client): vt100::set_size(0,0) and a
                // zero-size pty are both nonsense, so clamp to a 1x1 floor.
                let (cols, rows) = clamp_resize(cols, rows);
                let _ = a.pty.resize(hub_pty::PtySize { cols, rows });
                a.screen.resize(rows, cols);
                a.cur_cols = cols; a.cur_rows = rows;
                a.info.cols = cols; a.info.rows = rows;
            }
        }
    }
    // Clean exit: delete our record + socket (spec §9).
    SessionRecord::delete(&a.paths, a.id);
    let _ = std::fs::remove_file(a.paths.sock(a.id));
    Ok(())
}

fn handle_channel_frame(a: &mut RelayActor, cid: u64, frame: Frame) {
    match frame {
        Frame::Control(ControlMsg::Attach { .. }) => {
            a.attached.insert(cid);
            if let Some(tx) = a.channels.get(&cid) {
                // REPLAY = visible grid only (Plan 1 caveat).
                let _ = tx.send(encode_control(&ControlMsg::Replay { id: a.id, screen: a.screen.replay_bytes() }));
            }
            // SIGWINCH nudge for full-screen TUIs (Task 8 refines).
            nudge_repaint(a);
        }
        Frame::Control(ControlMsg::Detach { .. }) => { a.attached.remove(&cid); }
        Frame::Control(ControlMsg::List) => {
            // Reconciliation liveness probe: answer with our own info, no attach.
            if let Some(tx) = a.channels.get(&cid) {
                let _ = tx.send(encode_control(&ControlMsg::Sessions { sessions: vec![a.info.clone()] }));
            }
        }
        Frame::Data { bytes, .. } => { let _ = a.pty.write(&bytes); }
        Frame::Control(ControlMsg::Resize { cols, rows, .. }) | Frame::Control(ControlMsg::ClaimSize { cols, rows, .. }) => {
            if let Some(tx) = a.resize_in.as_ref() { let _ = tx.send((cols, rows)); }
        }
        Frame::Control(ControlMsg::Kill { .. }) => { sighup_shell(a); }
        _ => {}
    }
}

/// SIGWINCH nudge so full-screen TUIs repaint on attach (spec §7/§10).
/// Signals the shell's process; portable-pty's TIOCSWINSZ on resize is the
/// primary path — this is a best-effort extra for the same-size case.
fn nudge_repaint(a: &mut RelayActor) {
    if let Some(pid) = a.pty.child_pid() {
        unsafe { libc::kill(pid as i32, libc::SIGWINCH); }
    }
}

#[cfg(test)]
mod raw_term_tests {
    use super::RawTermGuard;
    use std::os::fd::AsRawFd;

    fn lflag(fd: i32) -> libc::tcflag_t {
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(fd, &mut t) }, 0, "tcgetattr");
        t.c_lflag
    }
    fn cooked_bits() -> libc::tcflag_t {
        (libc::ICANON as libc::tcflag_t) | (libc::ECHO as libc::tcflag_t)
    }

    // The guard clears ICANON+ECHO on the outer tty (so arrow keys pass through
    // instead of cooked-echoing as `^[[A`) and restores the original settings
    // on drop.
    #[test]
    fn raw_guard_clears_icanon_echo_and_restores_on_drop() {
        let pty = nix::pty::openpty(None, None).expect("openpty");
        let slave = pty.slave.as_raw_fd();

        // Baseline: force cooked (ICANON+ECHO on).
        let mut t: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::tcgetattr(slave, &mut t) }, 0);
        t.c_lflag |= cooked_bits();
        assert_eq!(unsafe { libc::tcsetattr(slave, libc::TCSANOW, &t) }, 0);
        assert_ne!(lflag(slave) & cooked_bits(), 0, "baseline should be cooked");

        {
            let _g = RawTermGuard::install_fd(slave).expect("install on a real tty");
            assert_eq!(
                lflag(slave) & cooked_bits(),
                0,
                "outer tty must be RAW while bridging (ICANON+ECHO cleared) or arrow keys echo as ^[[A"
            );
        } // drop restores

        assert_ne!(
            lflag(slave) & cooked_bits(),
            0,
            "guard must restore the original cooked settings on drop"
        );
    }

    // A non-tty fd (pipe) must be left completely untouched -> install is a no-op.
    #[test]
    fn raw_guard_is_noop_on_non_tty() {
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        assert!(RawTermGuard::install_fd(rd.as_raw_fd()).is_none());
    }
}

#[cfg(test)]
mod focus_guard_tests {
    use super::FocusReportGuard;
    use std::os::fd::AsRawFd;

    /// Drains everything currently buffered on `fd` (non-blocking) — used to
    /// read back what a guard wrote to the pty's master side.
    fn read_all_available(fd: i32) -> Vec<u8> {
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
        let mut out = Vec::new();
        let mut buf = [0u8; 256];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if n <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..n as usize]);
        }
        out
    }

    // Installing must enable DECSET 1004 on the real tty; dropping must
    // disable it again -- otherwise a killed relay would leave the user's
    // real terminal emitting focus-report escape codes into whatever runs
    // there next.
    #[test]
    fn focus_guard_enables_on_install_and_disables_on_drop() {
        let pty = nix::pty::openpty(None, None).expect("openpty");
        let slave = pty.slave.as_raw_fd();
        let master = pty.master.as_raw_fd();

        let g = FocusReportGuard::install_fd(slave).expect("install on a real tty");
        assert_eq!(read_all_available(master), b"\x1b[?1004h", "install must enable focus reporting");

        drop(g);
        assert_eq!(read_all_available(master), b"\x1b[?1004l", "drop must disable focus reporting");
    }

    // A non-tty fd (pipe) must be left completely untouched -> install is a no-op.
    #[test]
    fn focus_guard_is_noop_on_non_tty() {
        let (rd, _wr) = nix::unistd::pipe().expect("pipe");
        assert!(FocusReportGuard::install_fd(rd.as_raw_fd()).is_none());
    }
}
