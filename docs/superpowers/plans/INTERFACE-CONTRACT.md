# Interface Contract — Terminal Hub (v1)

Single source of truth for cross-crate/cross-plan types. All 4 implementation
plans MUST use these exact names, signatures, and shapes. Do not rename or
restructure — if a plan needs a change, it changes HERE first.

Spec: `docs/superpowers/specs/2026-07-19-terminal-hub-design.md`

---

## Workspace

Cargo workspace at repo root `hub/` (see spec §15). Rust edition 2021.
Shared deps pinned in root `Cargo.toml` `[workspace.dependencies]`:
- `tokio = { version = "1", features = ["full"] }`
- `serde = { version = "1", features = ["derive"] }`
- `serde_json = "1"`
- `portable-pty = "0.8"`
- `vt100 = "0.15"`
- `anyhow = "1"`
- `thiserror = "1"`
- `tracing = "0.1"`, `tracing-subscriber = "0.3"`

## Crate boundaries (who owns what)

- `hub-proto` — pure types + framing. NO IO, NO tokio. Plan 1.
- `hub-pty` — pty spawn/resize/io + child-death. Wraps `portable-pty`. Plan 1.
- `hub-term` — headless vt parse + scrollback + replay snapshot. Wraps `vt100`. Plan 1.
- `hub-transport` — async framed conn + unix listener (0600). Depends on `hub-proto` + tokio. Plan 1.
- `hub-relay` — binary; owns pty; detaches; serves viewers. Plan 2.
- `hub-daemon` — binary; router/registry; owns no pty. Plan 2.
- `hub-tui` — headless viewer for e2e/manual. Plan 2.
- `hub-cli` — `hub` binary: install/uninstall/attach --new/status/kill. Plan 3.
- `app/` — Tauri + xterm.js. Plan 4.

---

## `hub-proto` — TYPES (frozen)

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct SessionId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Origin { External, Hub }

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionInfo {
    pub id: SessionId,
    pub origin: Origin,
    pub title: String,
    pub pid: u32,
    pub started_unix: u64,
    pub cols: u16,
    pub rows: u16,
}

/// Control-plane messages (serialized as JSON in a control frame).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ControlMsg {
    // AUTH (F1): MUST be the FIRST frame on any hub socket (daemon hubd.sock
    // AND each relay's per-session <id>.sock) before any session op. Carries
    // the per-install secret token from `<HUB_DIR>/token` (0600). The server
    // also checks peer uid (SO_PEERCRED / getpeereid). Invalid token or uid
    // mismatch => connection closed, nothing processed. Token is NEVER logged.
    Hello { token: String },
    // relay -> daemon
    // F5: NO `env` field (removed). The daemon owns no pty and never read it;
    // shipping the shell's full environment (secrets/tokens) over the wire
    // for no benefit was pure exposure. The relay spawns its pty with the
    // real process environment directly, never via this message.
    Open { shell: String, cwd: String,
           cols: u16, rows: u16, term: String, origin: Origin, title: String },
    Opened { id: SessionId },
    Closed { id: SessionId, exit_code: Option<i32> },
    // hub <-> daemon
    List,
    Sessions { sessions: Vec<SessionInfo> },
    Attach { id: SessionId },
    Detach { id: SessionId },
    Replay { id: SessionId, screen: Vec<u8> }, // ANSI bytes reproducing current screen
    Resize { id: SessionId, cols: u16, rows: u16 },
    ClaimSize { id: SessionId, cols: u16, rows: u16 },
    Release { id: SessionId },
    Kill { id: SessionId },
    Error { message: String },
    Shutdown, // hub/CLI -> daemon: graceful daemon-process stop; relays survive.
}
```

## `hub-proto` — FRAMING (frozen)

Wire frame: `[len: u32 BE][tag: u8][payload...]` where `len` counts `tag + payload`.
- `tag = 0` → control: payload = `serde_json` of `ControlMsg`.
- `tag = 1` → data (hot path): payload = `[id: u64 BE][raw pty bytes...]`.

```rust
pub enum Frame {
    Control(ControlMsg),
    Data { id: SessionId, bytes: Vec<u8> },
}

pub fn encode_control(msg: &ControlMsg) -> Vec<u8>;      // full frame incl length
pub fn encode_data(id: SessionId, bytes: &[u8]) -> Vec<u8>; // full frame incl length

/// Streaming decoder that tolerates partial/split reads.
#[derive(Default)]
pub struct FrameDecoder { /* internal buffer */ }
impl FrameDecoder {
    pub fn push(&mut self, bytes: &[u8]);
    /// Returns next complete frame if buffered, else None. Call in a loop.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, ProtoError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("bad json: {0}")] Json(String),
    #[error("unknown tag {0}")] UnknownTag(u8),
    #[error("frame too large: {0}")] TooLarge(u32),
    #[error("malformed frame: {0}")] Malformed(String),
}
```
Max frame length constant: `pub const MAX_FRAME: u32 = 16 * 1024 * 1024;`

---

## `hub-pty` — API (frozen)

```rust
pub struct PtySize { pub cols: u16, pub rows: u16 }

pub struct Pty { /* holds portable_pty master + child handle */ }

pub struct PtyOutput {
    /// Blocking pty reads are bridged onto this channel by an internal thread.
    pub rx: std::sync::mpsc::Receiver<Vec<u8>>,
    /// Fires once with exit code when the child exits (EOF on pty).
    pub exit_rx: std::sync::mpsc::Receiver<Option<i32>>,
}

impl Pty {
    /// Spawn `shell` in a fresh pty. `env` overrides/extends inherited env.
    pub fn spawn(shell: &str, cwd: &str, env: &[(String, String)], size: PtySize)
        -> anyhow::Result<(Pty, PtyOutput)>;
    pub fn write(&mut self, bytes: &[u8]) -> anyhow::Result<()>;
    pub fn resize(&mut self, size: PtySize) -> anyhow::Result<()>;
    pub fn child_pid(&self) -> Option<u32>;
    /// SIGHUP + drop → ends the shell.
    pub fn kill(&mut self) -> anyhow::Result<()>;
}
```
Note: `portable-pty` I/O is blocking; the reader thread bridges to `rx`.
Plan 2 wraps `rx`/`exit_rx` into tokio via `spawn_blocking` or a bridge task.

---

## `hub-term` — API (frozen)

```rust
pub struct Screen { /* wraps vt100::Parser */ }

impl Screen {
    /// scrollback = max scrollback lines (default 10_000, configurable later).
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Screen;
    pub fn feed(&mut self, bytes: &[u8]);
    pub fn resize(&mut self, rows: u16, cols: u16);
    /// ANSI byte stream that reproduces the CURRENT screen when written to a
    /// fresh terminal. Used for REPLAY on attach.
    pub fn replay_bytes(&self) -> Vec<u8>;
}
```

---

## `hub-transport` — API (frozen)

```rust
/// Async framed connection over a unix stream.
pub struct FramedConn { /* tokio UnixStream + FrameDecoder */ }
impl FramedConn {
    pub fn new(stream: tokio::net::UnixStream) -> FramedConn;
    /// Reads until one full frame is available.
    pub async fn read_frame(&mut self) -> anyhow::Result<hub_proto::Frame>;
    /// Writes a pre-encoded frame (from encode_control/encode_data).
    pub async fn write_frame(&mut self, frame_bytes: &[u8]) -> anyhow::Result<()>;
}

/// Bind a unix listener with 0700 dir + 0600 socket perms.
pub async fn bind_listener(path: &std::path::Path) -> anyhow::Result<tokio::net::UnixListener>;
pub async fn connect(path: &std::path::Path) -> anyhow::Result<FramedConn>;
```

---

## Filesystem layout (runtime)

- Base dir: `~/.hub/` (dir perms `0700`).
- Daemon socket: `~/.hub/hubd.sock` (perms `0600`).
- Session records: `~/.hub/sessions/<id>.json` (matches `SessionInfo` + `sock`, `record_version`).
- Per-session sockets: `~/.hub/sessions/<id>.sock` (perms `0600`, explicitly chmodded by the relay after bind — F3).
- `~/.hub/sessions/` and `~/.hub/logs/` are each explicitly chmodded `0700` by `ensure_dirs`/`create_hub_tree`, not just the base dir (F3).
- Logs: `~/.hub/logs/` — lifecycle/errors only, NEVER pty bytes or env.

## Non-negotiable test gates (carried into later plans)

- **SPOF gate** (Plan 2): kill daemon → shells survive → reattach after daemon restart.
- **rc fail-safe gate** (Plan 3): daemon-down → plain shell works; `HUB_DISABLE=1` bypass; no double-inject; uninstall restores exact backup.

---

# ADDENDUM v2 — Cross-plan reconciliation (AUTHORITATIVE, overrides v1 above where they conflict)

Written after the 4 plans surfaced seams. Executors follow this over any per-plan divergence.

## A. Extra workspace deps (add to root `[workspace.dependencies]`)
- `libc = "0.2"` — used by relay teardown (`libc::kill(pid, SIGHUP)`) and detach (fork/setsid). Plan 1's `Pty::kill` stays SIGKILL; **clean shell teardown uses `libc::kill(pid, SIGHUP)` in `hub-relay`**, not `Pty::kill`.
- `clap = { version = "4", features = ["derive"] }` — CLI (Plan 3) + relay/daemon arg parsing.
- `tempfile = "3"` — test isolation (dev-dependency).

## B. Reverse-proxy topology (resolves "who listens where")
- **Viewers (hub GUI, hub-tui) connect ONLY to `~/.hub/hubd.sock`.** They never dial a relay directly.
- **Each relay listens on its own per-session socket** `~/.hub/sessions/<id>.sock`, **daemon-facing only** (perms 0600). Only the **daemon** connects to it, to proxy the secondary viewer's I/O.
- **Primary path is in-process, zero-hop:** the relay is the process running in the terminal; it owns the pty and relays outer-terminal⇄pty natively. The daemon is NOT in the primary path.
- **Daemon = reverse proxy** for secondary viewers: `hub → hubd.sock → daemon → relay's per-session socket → pty`. This is the ~0.2ms secondary hop.
- **Re-adoption on daemon restart:** daemon re-reads record files, reconnects to each live relay socket, preserves the `id` from the record — **relays do NOT re-`Open`.** This is what makes the SPOF gate meaningful (kill daemon → relays+shells live → restart → re-adopt).
- Note: this means N+1 unix sockets exist (N relay + 1 daemon), but only the **1 daemon socket is viewer-facing**; relay sockets are internal/daemon-only. The "one socket" property refers to the viewer/discovery surface.

## C. Session records & reconciliation buckets (single canonical definition)
`~/.hub/sessions/<id>.json` = `{ record_version: u32, ...SessionInfo fields, sock: String }` where `sock` = the relay's per-session socket path. `SessionInfo.pid` = the **relay** pid (the process the SPOF test verifies survives).
Buckets (daemon live-set is authoritative; record files are disk truth):
- **healthy** = record present AND `sock` connectable (relay live) AND in daemon's session list.
- **ghost** = record present BUT `sock` dead → relay crashed → offer cleanup/delete.
- **orphan** = `sock`/relay live BUT no record file (or daemon session with no record) → offer adopt/kill.

## D. `hub-relay` binary — canonical CLI (Plans 2, 3, 4 all target this)
```
hub-relay --origin <external|hub> --shell <path> --cwd <path> --term <name> \
          --cols <n> --rows <n> --daemon-sock <path> [--detach]
```
- `--origin external` → NOT detached (stays foreground; outer-terminal close SIGHUPs it → External teardown). `exec`'d in place of the interactive shell by the rc path.
- `--origin hub --detach` → fully daemonizes (setsid + double-fork + reparent to init) → survives app-close AND daemon-crash. Teardown only on explicit `Kill` or shell exit.

## E. `hub` CLI wrappers (Plan 3) over `hub-relay`
- `hub attach --new` (rc-injection path) → `exec hub-relay --origin external ...`; on any failure/daemon-down → falls through to `exec $SHELL` (fail-safe).
- `hub attach --new --origin hub` (or GUI "new session") → spawns `hub-relay --origin hub --detach`.
- Hub-origin spawn is **CLI/daemon-driven, NOT a wire message.** There is no `ControlMsg::Spawn`.

## F. Control-message semantics (clarifications, + one addition)
- **INPUT / OUTPUT are `Frame::Data { id, bytes }` (tag=1), NOT `ControlMsg`.** Spec §7's "INPUT/OUTPUT" naming = data frames. `send_input` → `encode_data`; streamed output → `Frame::Data`.
- **`Kill { id }` is acked:** daemon replies `Closed { id, exit_code }` on success, or `Error { message }` on failure. `hub kill` / uninstall treat a clean close as best-effort success.
- **ADD `ControlMsg::Shutdown`** (hub/CLI → daemon): graceful daemon stop for `hub uninstall` / `hub stop`. Daemon drains, detaches from relays (does NOT kill them — relays survive), exits. (Amends the frozen `ControlMsg` enum in §hub-proto.)
- `Detach{id}` = stop viewing (viewer leaves; session unaffected). `Release{id}` = drop a size claim (available; not wired to a v1 GUI gesture). `Resize` = plain pty resize; `ClaimSize` = the focus-follows-size mechanism (§7). GUI wires focus/drag → `ClaimSize`.

## G. Plan-1 consumption facts (not contract changes, but pin them)
- `FramedConn` is for one-shot request/reply. For concurrent read+write (daemon/relay/tui) use `UnixStream::into_split()` + `hub_proto::FrameDecoder` directly.
- `PtyOutput.rx`/`exit_rx` are `Send` (moved into std bridge threads); `exit_rx` fires exactly once with the child exit code.
- **Arg-order footgun:** `PtySize { cols, rows }` is cols-first; `Screen::new(rows, cols, scrollback)` and `Screen::resize(rows, cols)` are **rows-first.** Do not transpose.
- Buffer size: fixed at relay spawn (`Screen::new` scrollback). GUI's buffer-size setting persists to `~/.hub/config.json` and applies to **new** sessions' xterm scrollback only — it cannot retro-resize a running relay's vt ring.

## H. Test isolation convention
- `HUB_DIR` env var overrides the `~/.hub` base dir (defaults to `~/.hub` when unset). All binaries + tests honor it, so gates run under a throwaway dir.

## I. Plan-1 carryover (built + final-reviewed; Plan 2 MUST absorb these)
Plan 1 foundations are complete and reviewed. Facts + follow-ups for Plan 2 (relay/daemon):
- `hub-pty` reader thread now **retries EINTR** (does not break) — safe under the relay's signal work (fork/setsid/SIGHUP/SIGWINCH/SIGCHLD). Do not reintroduce a catch-all break.
- `Pty::kill()` is **SIGKILL** (portable-pty `Child::kill`), doc now says so. Clean External-origin teardown MUST use `libc::kill(pid, SIGHUP)` in `hub-relay` (use `Pty::child_pid()`), not `Pty::kill()`.
- **`bind_listener` only chmods the immediate parent dir to 0700**, not the whole created chain. On the daemon-never-ran-yet path (fresh install, external session binds a per-session socket before `~/.hub` exists), the base dir can be left at umask default. Plan 2 MUST ensure `~/.hub` itself is 0700 independently (daemon creates it 0700 on startup; relay verifies/creates before binding).
- `PtyOutput.exit_rx` fires up to **~20ms after** `rx` closes (20ms `try_wait` poll), not synchronized with pty EOF. The tokio bridge must not assume exit arrives with/before EOF.
- `FrameDecoder` uses front-`drain` (O(K²) for K frames buffered from one read). Fine at 8KiB reads; if Plan 2's data hot path shows it, switch to a read cursor. Deferred, not required.
- Add a `Drop` for `Pty` (kill + join threads) in Plan 2 to avoid a detached waiter polling forever if a `Pty` is dropped without `kill()`.
- `Pty::child_pid() -> Option<u32>` is exposed (relay uses it for SIGHUP + as the SPOF-surviving pid in `SessionInfo.pid`).

## L. GUI backend connection model (Plan 4 — ARCH DECISION, user-approved)
The daemon's viewer model is **one connection = one attached session** (verified: `hub-daemon` `drive_viewer_attached` locks a connection to the session it `Attach`ed, routes Input/Kill/Resize by the connection's attached id, `Detach`=>closes the connection). The GUI backend therefore uses **Approach A — per-tile connections** (NOT one multiplexed connection). Chosen for quick updates + true concurrency (parallel per-tile drain, per-tile failure isolation), zero daemon change (reuses the proven model that `hub-tui`/`hub-cli` already use), and Windows-safe (named-pipe multi-instance; 255-instance cap never approached by a terminal GUI).

**GUI backend = a connection manager (`hub/app/src-tauri`), NOT a single shared `DaemonClient`:**
- `attach(id)` → open a NEW authenticated connection (`Hello`→`Attach{id}`), spawn a reader task that emits that session's `Replay`/`Data`/`Closed`/`Error` as Tauri events tagged with `id`; keep it in a `session_id → ViewerConn` map with an input channel. Idempotent (re-attach = no-op/refocus).
- `send_input(id,bytes)`/`resize`/`claim_size`/`kill` → routed on **that session's** connection from the map. `detach(id)` → close/remove that session's connection (never kills). `kill(id)` on a not-currently-viewed session (ghost/orphan) → short-lived connection doing `Hello`→`Attach{id}`→`Kill{id}` (mirrors `hub-cli::daemon_client::kill_session`).
- `list_sessions`/`reconcile_sessions` → short-lived connection `Hello`→`List`→`Sessions` (+ record-file diff), then close. `spawn_session` → launch `hub-relay --origin hub --detach` (no daemon connection).
- **Reconnect is per-tile:** a tile whose connection drops (daemon crash / relay exit → `Closed`/EOF) surfaces to the UI; re-`attach` opens a fresh connection (this is the GUI's SPOF-reconnect path). NO app-wide single-connection to lose.
- MUST be verified against the REAL `hub-daemon`+relay (not just the mock), preserving auth (every connection sends `Hello{token}` first).

## J. Plan-2 carryover (built + final-reviewed "ready to build on"; Plans 3/4 MUST absorb these)
Plan 2 = working headless multiplexer, SPOF gate green (38/38 tests). Fix obligations:

**PRE-PLAN-3 hygiene (do before/with install work):**
- Fix the persistent unused-import warning at `hub-relay/src/main.rs:1` (dead `mod args`).
- Gate the `--selftest-ppid` hook out of the release `hub-relay` binary (`#[cfg(debug_assertions)]` or a `selftest` feature).
- **reconcile id-floor bug:** daemon startup seeds `next_id` only from the Healthy bucket; it MUST seed past ALL live socket ids (orphan + ghost too), else a new Open can be assigned a live orphan's id and hijack its socket. Fix in `hub-daemon/src/server.rs run()`.
- Add defensive `paths.ensure_dirs()` in `run_relay` before binding `<id>.sock`/writing the record (a directly-spawned relay on a fresh `~/.hub` must not fail; also §I base-dir-0700).
- **Daemon singleton guard:** `bind_listener` unconditionally unlinks a stale `hubd.sock`, so a 2nd daemon silently steals it from a live one. Plan 3 install adds a pidfile/flock liveness check alongside launchd/systemd.
- Handle `ControlMsg::Shutdown` (ADDENDUM F) in the daemon (currently hits the `other =>` warn path) for clean `hub uninstall`/`hub stop`.

**PRE-PLAN-4 GUI (BLOCKING — Plan 4 is the real lagging-viewer scenario):**
- **Backpressure:** replace unbounded viewer/output channels with bounded + newest-wins drop/coalesce. The bounded send must NOT run while holding the registry `Mutex` (else one slow viewer serializes all sessions). Relay/shell paths stay as-is (shell-never-stalls already holds).
- **Move External primary stdout off the actor loop:** `run_actor`'s `RelayEvent::Output` awaits `stdout.write_all().await` inline — a Ctrl-S'd outer terminal freezes the whole actor (all viewers + input + resize). Offload to its own task with a bounded buffer (like `writer_sink`).
- Send `Detach` to the relay on last-viewer-leave (daemon `detach_viewer`) so a zero-viewer relay stops streaming; pairs with the backpressure fix.
- GUI INFO: a just-attached viewer may receive `Data` before its `Replay` (harmless full-screen repaint) — xterm.js integration should tolerate it.

**Defer indefinitely (cleanup sweep, non-gating):** reconcile blocking-fs + sequential 300ms probes; pty-spawned-before-daemon-dial leak on setup failure; drive_relay pid-0 sentinel; record-vs-socket test load race; error-path `exit(1)`→`_exit(1)`; Output/Exit teardown ordering (last chunk drop); `own_stdio` bool→enum; RelayActor pub→pub(crate); add `impl Drop for Pty` (kill+join).

## K. Plan-3 carryover (built + final-reviewed "ready to build on"; fix before Plan 4 SHIPS)
Plan 3 = working `hub` CLI (attach fail-safe, install/uninstall, autostart, status, kill). RC fail-safe gate GREEN (mutation-verified). Workspace 98/98.

**BEFORE PLAN 4 SHIPS (not blocking Plan 4 start):**
- **Manifest crash-safety (Important):** `hub install` persists the manifest only at the END of `run()`. A crash after an rc edit but before `manifest::save` leaves a guarded hub block + orphaned backup with an EMPTY on-disk manifest → uninstall can't remove it, and a later install won't re-record it (untracked forever). Fix: persist the manifest immediately after `inject_all` (and re-save after autostart), or append-persist per touched file.
- **§J RESOLVED — `ControlMsg::Shutdown` handler:** `hub-daemon/src/server.rs::handle_conn` now accepts `Shutdown` as a first frame: it triggers a clean process exit (accept loop is signaled to stop via a shutdown flag/channel, `run()` returns) without touching relay connections — relays keep running. `daemon_client::shutdown_daemon` sends `ControlMsg::Shutdown` as the primary daemon-process stop (best-effort; connect failure = daemon already down), with autostart-removal remaining the fallback in `hub uninstall`. Covered by `crates/hub-daemon/tests/shutdown.rs`.
- **§J RESOLVED — daemon singleton guard:** `hub-daemon/src/server.rs::run` now acquires an exclusive `flock` (LOCK_EX | LOCK_NB, via `nix::fcntl::flock`) on `<HUB_DIR>/hubd.lock` BEFORE calling `bind_listener`, and holds the lock file handle for the daemon's lifetime. A 2nd daemon under the same HUB_DIR fails the lock, logs "daemon already running", and exits non-zero without touching `hubd.sock` — `bind_listener`'s unconditional unlink-and-bind is now safe because only the lock-holder ever reaches it. Covered by `crates/hub-daemon/tests/singleton.rs`.

**Also still pending from §J (PRE-PLAN-4 GUI, BLOCKING for GUI — from Plan 2):** bounded/coalescing viewer channels (backpressure); move External primary stdout off the actor loop; send Detach to relay on last-viewer-leave. These remain unaddressed and are the real lagging-viewer fixes the GUI needs.

**Defer (opportunistic cleanup):** HUB_DISABLE defense-in-depth in run_attach; backup filename sub-second uniqueness; detect_shells home param; remove_autostart error surfacing; vanished-backup → surgical-removal fallback; drop `#![allow(dead_code)]` in paths.rs/autostart.rs once confirmed all-wired; 3 pre-existing test-target warnings (unused mut in hub-e2e teardown/two_viewers; unused Origin import in hub-daemon teardown_origin).
