# Install / CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `hub` CLI binary (`hub-cli` crate) — `attach --new`, `install`, `uninstall`, `status`, `kill` — plus the cross-shell fail-safe rc-injection machinery, so an interactive shell is auto-captured after a consent-gated one-time install and a terminal is **never** left broken.

**Architecture:** `hub-cli` produces the `hub` binary. `hub attach --new` is what the injected shell-rc snippet calls; it does a synchronous, non-hanging daemon reachability probe and then `exec`s `hub-relay` (Plan 2) to take over the terminal, or falls through to `exec $SHELL` if anything is wrong. `hub install/uninstall` edit shell rc files behind marker-guarded blocks with byte-for-byte backups + an install manifest, manage OS autostart (launchd/systemd), and own the `~/.hub` tree. `hub status/kill` talk to the daemon over `hub-transport`. The decision logic in every destructive/fragile path is factored into **pure functions** so it can be unit-tested without touching the real environment; shell behavior is tested by driving real `bash`/`zsh` inside a `portable-pty` pseudo-terminal under a throwaway `$HOME`.

**Tech Stack:** Rust (edition 2021), `clap = "4"` (derive) for args, `hub-proto` + `hub-transport` (Plans 1 & 2) for daemon IO, `tokio` (status/kill only — attach stays sync), `libc` (`TIOCGWINSZ` + `exec`), `sha2` (file hashes), `dirs` (home dir), `serde`/`serde_json` (manifest), dev-deps `portable-pty` + `tempfile` (shell test harness). Autostart via launchd plist (macOS) / systemd user unit (Linux).

## Global Constraints

Copied verbatim from `docs/superpowers/plans/INTERFACE-CONTRACT.md`. Every task's requirements implicitly include this section.

- Cargo workspace at repo root `hub/` (see spec §15). Rust edition 2021.
- Shared deps pinned in root `Cargo.toml` `[workspace.dependencies]`:
  - `tokio = { version = "1", features = ["full"] }`
  - `serde = { version = "1", features = ["derive"] }`
  - `serde_json = "1"`
  - `portable-pty = "0.8"`
  - `vt100 = "0.15"`
  - `anyhow = "1"`
  - `thiserror = "1"`
  - `tracing = "0.1"`, `tracing-subscriber = "0.3"`
- `hub-cli` — `hub` binary: install/uninstall/attach --new/status/kill. Plan 3.
- Filesystem layout (runtime):
  - Base dir: `~/.hub/` (dir perms `0700`).
  - Daemon socket: `~/.hub/hubd.sock` (perms `0600`).
  - Session records: `~/.hub/sessions/<id>.json` (matches `SessionInfo` + `sock`, `record_version`).
  - Logs: `~/.hub/logs/` — lifecycle/errors only, NEVER pty bytes or env.
- `hub-proto` types are **frozen**: `SessionId(pub u64)`, `Origin::{External, Hub}`, `SessionInfo { id, origin, title, pid, started_unix, cols, rows }`, `ControlMsg::{Open, Opened, Closed, List, Sessions, Attach, Detach, Replay, Resize, ClaimSize, Release, Kill, Error}`, `Frame::{Control(ControlMsg), Data{id,bytes}}`, `encode_control(&ControlMsg) -> Vec<u8>`, `encode_data(SessionId,&[u8]) -> Vec<u8>`.
- `hub-transport` **frozen**: `FramedConn::{read_frame() -> anyhow::Result<Frame>, write_frame(&[u8]) -> anyhow::Result<()>}`, `bind_listener(&Path)`, `connect(&Path) -> anyhow::Result<FramedConn>`.
- **Non-negotiable rc fail-safe gate (Plan 3):** daemon-down → plain shell works; `HUB_DISABLE=1` bypass; no double-inject; uninstall restores exact backup.

### Additional workspace deps this plan adds

Add these to the root `hub/Cargo.toml` `[workspace.dependencies]` (Task 1 does this):

```toml
clap = { version = "4", features = ["derive"] }
libc = "0.2"
sha2 = "0.10"
dirs = "5"
tempfile = "3"
```

(`portable-pty = "0.8"` is already pinned in the contract; `hub-cli` uses it only as a dev-dependency for the shell test harness.)

### Plan-2 (daemon/relay) behavior this plan assumes

These are the seams Plan 3 depends on. **Reconcile with Plan 2 before executing.** Each is annotated where first used.

- **A1 — `hub-relay` binary, external-foreground mode.** `hub-relay --origin external --shell <S> --cwd <C> --term <T> --cols <n> --rows <n> --daemon-sock <P>`. When `exec`'d in place of the interactive shell, it opens the inner pty, spawns `$SHELL`, sends `ControlMsg::Open{ origin: External, ... }` to the daemon, and takes over the **current** terminal as the primary viewer (native zero-hop I/O per spec §3.2). It inherits `HUB_ACTIVE=1` in its env. Lives in the outer terminal (NOT double-forked) so closing the outer terminal SIGHUPs it → session dies (spec §5/§7 External teardown). The `hub-relay` binary sits **beside** the `hub` binary on disk.
- **A2 — Daemon reachability = a connectable socket.** A successful `UnixStream::connect(~/.hub/hubd.sock)` means the daemon is up. A missing socket file or `ECONNREFUSED` (stale socket, no listener) means down. Unix-domain `connect` returns immediately in both cases, so the probe cannot hang.
- **A3 — `ControlMsg::List` → `ControlMsg::Sessions{ sessions: Vec<SessionInfo> }`.** Used by `hub status`.
- **A4 — `ControlMsg::Kill{id}` is acked** by the daemon with `ControlMsg::Closed{id, ..}` on success or `ControlMsg::Error{message}` on failure. `hub kill` treats any other single reply, or a clean connection close, as best-effort success.
- **A5 — `hub-daemon` binary runs the daemon in the foreground** when launched with no arguments (launchd/systemd supervise it). It sits beside the `hub` binary on disk. `hub install` records its path and points the autostart entry at it.
- **A6 — Session record files** at `~/.hub/sessions/<id>.json` deserialize into `SessionInfo` plus a `sock: String` field (the relay's per-session socket path, per spec §9 auto-discovery) and a `record_version` field. `hub status` probes that `sock` with `UnixStream::connect` to distinguish ghosts (record present, socket dead) from live sessions.

---

## File structure (created by this plan)

```
hub/crates/hub-cli/
├─ Cargo.toml
├─ src/
│  ├─ main.rs          # clap parse + dispatch (attach = sync; others = tokio runtime)
│  ├─ cli.rs           # clap Args/Subcommand definitions
│  ├─ paths.rs         # ~/.hub tree paths, home dir, daemon sock path, sibling-binary locate
│  ├─ attach.rs        # `attach --new`: gather inputs, plan_attach(), exec relay or $SHELL
│  ├─ snippet.rs       # rc snippet text (include_str! of install/*.sh) + marker constants
│  ├─ rcfile.rs        # per-shell rc file selection incl. bash .bashrc/.bash_profile split
│  ├─ manifest.rs      # install-manifest read/write + TouchedFile/AutostartEntry
│  ├─ install.rs       # `install`: dirs, backup, inject, idempotent, consent gate
│  ├─ autostart.rs     # launchd plist / systemd user unit generate + (un)load (cfg-gated)
│  ├─ uninstall.rs     # `uninstall`: restore/surgical-remove, dry-run, tear down
│  ├─ daemon_client.rs # connect + List/Kill helpers over hub-transport
│  ├─ status.rs        # `status`: reconciliation buckets (healthy/ghost/orphan)
│  └─ kill.rs          # `kill <id>`
└─ tests/
   ├─ common/mod.rs        # pty harness + fake-hub + temp-HOME helpers
   ├─ attach_failsafe.rs   # plan_attach() unit gate
   ├─ rcfile_split.rs      # bash/zsh file-selection unit tests
   ├─ install_idempotent.rs
   ├─ uninstall_restore.rs
   ├─ autostart_content.rs
   ├─ reconcile.rs         # status bucketing unit tests
   ├─ daemon_client_fake.rs# List/Kill against an in-process fake daemon socket
   └─ rc_gate.rs           # THE rc fail-safe gate: drives real bash/zsh in a pty

hub/install/
├─ zsh-snippet.sh          # canonical zsh block (WITH markers)
├─ bash-snippet.sh         # canonical bash block (WITH markers)
└─ bash-profile-bridge.sh  # login-shell → ~/.bashrc bridge block (WITH its own markers)
```

---

### Task 1: Crate scaffold, clap skeleton, path helpers

**Files:**
- Modify: `hub/Cargo.toml` (workspace `members` + `[workspace.dependencies]` additions)
- Create: `hub/crates/hub-cli/Cargo.toml`
- Create: `hub/crates/hub-cli/src/main.rs`
- Create: `hub/crates/hub-cli/src/cli.rs`
- Create: `hub/crates/hub-cli/src/paths.rs`
- Test: `hub/crates/hub-cli/src/paths.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing yet (Plans 1 & 2 crates referenced but not called here).
- Produces:
  - `paths::hub_dir(home: &Path) -> PathBuf` (`home/.hub`), `paths::sessions_dir`, `paths::logs_dir`, `paths::backups_dir`, `paths::manifest_path`, all `(home: &Path) -> PathBuf`.
  - `paths::daemon_sock_path(home: &Path) -> PathBuf` — honors `$HUB_SOCK`, else `home/.hub/hubd.sock`.
  - `paths::home_dir() -> PathBuf` — `$HOME` or `dirs::home_dir()`.
  - `paths::locate_sibling(name: &str) -> Option<PathBuf>` — `name` next to `current_exe()`.
  - `cli::Cli` (clap derive) with subcommands `Attach{ new: bool }`, `Install{ yes: bool }`, `Uninstall{ yes: bool, dry_run: bool }`, `Status`, `Kill{ id: u64 }`.

- [ ] **Step 1: Register the crate + deps in the workspace**

Edit `hub/Cargo.toml`. Add `"crates/hub-cli"` to `[workspace] members`, and append to `[workspace.dependencies]`:

```toml
clap = { version = "4", features = ["derive"] }
libc = "0.2"
sha2 = "0.10"
dirs = "5"
tempfile = "3"
```

- [ ] **Step 2: Write `hub/crates/hub-cli/Cargo.toml`**

```toml
[package]
name = "hub-cli"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "hub"
path = "src/main.rs"

[dependencies]
hub-proto = { path = "../hub-proto" }
hub-transport = { path = "../hub-transport" }
clap = { workspace = true }
tokio = { workspace = true }
anyhow = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
libc = { workspace = true }
sha2 = { workspace = true }
dirs = { workspace = true }

[dev-dependencies]
portable-pty = { workspace = true }
tempfile = { workspace = true }
```

- [ ] **Step 3: Write the failing test for path helpers**

Create `hub/crates/hub-cli/src/paths.rs`:

```rust
use std::path::{Path, PathBuf};

pub fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME") {
        return PathBuf::from(h);
    }
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn hub_dir(home: &Path) -> PathBuf { home.join(".hub") }
pub fn sessions_dir(home: &Path) -> PathBuf { hub_dir(home).join("sessions") }
pub fn logs_dir(home: &Path) -> PathBuf { hub_dir(home).join("logs") }
pub fn backups_dir(home: &Path) -> PathBuf { hub_dir(home).join("backups") }
pub fn manifest_path(home: &Path) -> PathBuf { hub_dir(home).join("install-manifest.json") }

pub fn daemon_sock_path(home: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("HUB_SOCK") {
        return PathBuf::from(p);
    }
    hub_dir(home).join("hubd.sock")
}

pub fn locate_sibling(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let cand = exe.parent()?.join(name);
    cand.exists().then_some(cand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn tree_paths_are_under_dot_hub() {
        let home = Path::new("/tmp/fakehome");
        assert_eq!(hub_dir(home), Path::new("/tmp/fakehome/.hub"));
        assert_eq!(sessions_dir(home), Path::new("/tmp/fakehome/.hub/sessions"));
        assert_eq!(manifest_path(home), Path::new("/tmp/fakehome/.hub/install-manifest.json"));
    }

    #[test]
    fn daemon_sock_defaults_under_dot_hub() {
        std::env::remove_var("HUB_SOCK");
        let home = Path::new("/tmp/fakehome");
        assert_eq!(daemon_sock_path(home), Path::new("/tmp/fakehome/.hub/hubd.sock"));
    }
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test -p hub-cli paths:: -- --nocapture`
Expected: FAIL — crate does not compile yet (`main.rs`/`cli.rs` missing).

- [ ] **Step 5: Write `cli.rs` (clap skeleton)**

Create `hub/crates/hub-cli/src/cli.rs`:

```rust
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(name = "hub", about = "Terminal Hub control CLI")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Capture the current terminal (called by the injected shell-rc snippet).
    Attach {
        /// Spawn/attach a NEW relay for this terminal.
        #[arg(long)]
        new: bool,
    },
    /// One-time consent-gated setup (rc injection + autostart + ~/.hub).
    Install {
        /// Skip the interactive confirmation prompt.
        #[arg(long)]
        yes: bool,
    },
    /// Full clean: restore rc files, stop daemon, remove autostart + ~/.hub.
    Uninstall {
        #[arg(long)]
        yes: bool,
        /// List everything that would be touched, change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// List sessions (healthy / ghost / orphan buckets).
    Status,
    /// Kill a session by id.
    Kill { id: u64 },
}
```

- [ ] **Step 6: Write `main.rs` (dispatch stubs)**

Create `hub/crates/hub-cli/src/main.rs`. Attach stays synchronous (no runtime → cannot hang); other commands get a tokio runtime.

```rust
mod attach;
mod autostart;
mod cli;
mod daemon_client;
mod install;
mod kill;
mod manifest;
mod paths;
mod rcfile;
mod snippet;
mod status;
mod uninstall;

use clap::Parser;
use cli::{Cli, Command};

fn main() {
    let cli = Cli::parse();
    match cli.command {
        // Never returns; must not build an async runtime before the exec.
        Command::Attach { new: _ } => attach::run_attach(),
        other => {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            let code = rt.block_on(async move {
                let home = paths::home_dir();
                let res = match other {
                    Command::Install { yes } => install::run(&home, yes),
                    Command::Uninstall { yes, dry_run } => uninstall::run(&home, yes, dry_run).await,
                    Command::Status => status::run(&home).await,
                    Command::Kill { id } => kill::run(&home, id).await,
                    Command::Attach { .. } => unreachable!(),
                };
                match res {
                    Ok(()) => 0,
                    Err(e) => { eprintln!("hub: {e:#}"); 1 }
                }
            });
            std::process::exit(code);
        }
    }
}
```

Create empty-but-compiling stubs so the crate builds (each is fleshed out in its own task):

```rust
// attach.rs
pub fn run_attach() -> ! { eprintln!("attach: not yet implemented"); std::process::exit(1) }
```
```rust
// snippet.rs, rcfile.rs, manifest.rs, install.rs, autostart.rs, uninstall.rs,
// daemon_client.rs, status.rs, kill.rs — create each with a minimal placeholder:
// install.rs
use std::path::Path;
pub fn run(_home: &Path, _yes: bool) -> anyhow::Result<()> { anyhow::bail!("install: not yet implemented") }
```
```rust
// uninstall.rs
use std::path::Path;
pub async fn run(_home: &Path, _yes: bool, _dry_run: bool) -> anyhow::Result<()> { anyhow::bail!("uninstall: not yet implemented") }
```
```rust
// status.rs
use std::path::Path;
pub async fn run(_home: &Path) -> anyhow::Result<()> { anyhow::bail!("status: not yet implemented") }
```
```rust
// kill.rs
use std::path::Path;
pub async fn run(_home: &Path, _id: u64) -> anyhow::Result<()> { anyhow::bail!("kill: not yet implemented") }
```
```rust
// daemon_client.rs, autostart.rs, snippet.rs, rcfile.rs, manifest.rs — start empty (`// filled in Task N`).
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p hub-cli paths::`
Expected: PASS (2 tests). Also `cargo build -p hub-cli` compiles and `cargo run -p hub-cli -- --help` prints the subcommand list.

- [ ] **Step 8: Checkpoint**

Run: `cargo test -p hub-cli paths:: && cargo run -p hub-cli -- --help`
Expected: 2 tests pass; help text lists `attach`, `install`, `uninstall`, `status`, `kill`. No commit.

---

### Task 2: `hub attach --new` — fail-safe decision + exec

The single most safety-critical CLI path. Decision logic is a **pure function** (`plan_attach`) so we test the fail-safe without ever `exec`ing.

**Files:**
- Modify: `hub/crates/hub-cli/src/attach.rs`
- Test: `hub/crates/hub-cli/tests/attach_failsafe.rs`

**Interfaces:**
- Consumes: `paths::{home_dir, daemon_sock_path, locate_sibling}` (Task 1); Plan 2 seam **A1** (`hub-relay` external-foreground CLI) and **A2** (connectable socket = daemon up).
- Produces:
  - `attach::AttachInputs { shell, cwd, term, cols, rows, hub_active, relay_path, daemon_sock, daemon_up }`.
  - `attach::AttachAction::{ ExecShell(String), ExecRelay{ relay: PathBuf, args: Vec<String>, env: Vec<(String,String)> } }`.
  - `attach::plan_attach(&AttachInputs) -> AttachAction` (pure).
  - `attach::run_attach() -> !` (gathers inputs, executes the action; never returns).

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/attach_failsafe.rs`:

```rust
use hub_cli::attach::{plan_attach, AttachAction, AttachInputs};
use std::path::PathBuf;

fn base() -> AttachInputs {
    AttachInputs {
        shell: "/bin/zsh".into(),
        cwd: "/home/u".into(),
        term: "xterm-256color".into(),
        cols: 120,
        rows: 40,
        hub_active: false,
        relay_path: Some(PathBuf::from("/opt/hub/hub-relay")),
        daemon_sock: PathBuf::from("/home/u/.hub/hubd.sock"),
        daemon_up: true,
    }
}

#[test]
fn daemon_down_falls_through_to_shell() {
    let i = AttachInputs { daemon_up: false, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(s) if s == "/bin/zsh"));
}

#[test]
fn already_active_falls_through_to_shell() {
    let i = AttachInputs { hub_active: true, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(s) if s == "/bin/zsh"));
}

#[test]
fn missing_relay_binary_falls_through_to_shell() {
    let i = AttachInputs { relay_path: None, ..base() };
    assert!(matches!(plan_attach(&i), AttachAction::ExecShell(_)));
}

#[test]
fn healthy_path_execs_relay_with_external_origin() {
    match plan_attach(&base()) {
        AttachAction::ExecRelay { relay, args, env } => {
            assert_eq!(relay, PathBuf::from("/opt/hub/hub-relay"));
            assert!(args.windows(2).any(|w| w == ["--origin", "external"]));
            assert!(args.windows(2).any(|w| w == ["--shell", "/bin/zsh"]));
            assert!(args.windows(2).any(|w| w == ["--cols", "120"]));
            assert!(args.windows(2).any(|w| w == ["--rows", "40"]));
            assert!(env.iter().any(|(k, v)| k == "HUB_ACTIVE" && v == "1"));
        }
        other => panic!("expected ExecRelay, got {other:?}"),
    }
}
```

Note: the test imports `hub_cli::attach::*`, so the crate needs a `lib` target. Add to `hub/crates/hub-cli/Cargo.toml`:

```toml
[lib]
name = "hub_cli"
path = "src/lib.rs"
```

Create `hub/crates/hub-cli/src/lib.rs` re-exporting the modules the tests use:

```rust
pub mod attach;
pub mod autostart;
pub mod install;
pub mod manifest;
pub mod paths;
pub mod rcfile;
pub mod reconcile;   // added in Task 8
pub mod snippet;
```

And change `main.rs` to use the library crate instead of re-declaring modules: replace the `mod ...;` block with `use hub_cli::{attach, install, paths, status, uninstall, kill, daemon_client};` — keep `status`, `kill`, `daemon_client` as `pub mod` in `lib.rs` too. (Add every module referenced by `main.rs` to `lib.rs` as it is created.)

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli --test attach_failsafe`
Expected: FAIL — `plan_attach` unimplemented (still the placeholder).

- [ ] **Step 3: Implement `attach.rs`**

Replace `hub/crates/hub-cli/src/attach.rs`:

```rust
use crate::paths;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AttachInputs {
    pub shell: String,
    pub cwd: String,
    pub term: String,
    pub cols: u16,
    pub rows: u16,
    pub hub_active: bool,
    pub relay_path: Option<PathBuf>,
    pub daemon_sock: PathBuf,
    pub daemon_up: bool,
}

#[derive(Debug)]
pub enum AttachAction {
    /// Fall through: replace this process with the user's plain shell.
    ExecShell(String),
    /// Replace this process with the relay (external capture).
    ExecRelay {
        relay: PathBuf,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
}

/// Pure decision. Any doubt → ExecShell so the terminal is never broken.
pub fn plan_attach(i: &AttachInputs) -> AttachAction {
    if i.hub_active {
        return AttachAction::ExecShell(i.shell.clone());
    }
    if !i.daemon_up {
        return AttachAction::ExecShell(i.shell.clone());
    }
    let relay = match &i.relay_path {
        Some(p) => p.clone(),
        None => return AttachAction::ExecShell(i.shell.clone()),
    };
    let args = vec![
        "--origin".into(), "external".into(),
        "--shell".into(), i.shell.clone(),
        "--cwd".into(), i.cwd.clone(),
        "--term".into(), i.term.clone(),
        "--cols".into(), i.cols.to_string(),
        "--rows".into(), i.rows.to_string(),
        "--daemon-sock".into(), i.daemon_sock.display().to_string(),
    ];
    AttachAction::ExecRelay {
        relay,
        args,
        env: vec![("HUB_ACTIVE".into(), "1".into())],
    }
}

fn terminal_size() -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::ioctl(libc::STDIN_FILENO, libc::TIOCGWINSZ, &mut ws) };
    if r == 0 && ws.ws_col > 0 {
        Some((ws.ws_col, ws.ws_row))
    } else {
        None
    }
}

fn gather_inputs() -> AttachInputs {
    let home = paths::home_dir();
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into());
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/".into());
    let term = std::env::var("TERM")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "xterm-256color".into());
    let (cols, rows) = terminal_size().unwrap_or((80, 24));
    let daemon_sock = paths::daemon_sock_path(&home);
    // A2: connectable unix socket == daemon up. Returns immediately either way.
    let daemon_up = std::os::unix::net::UnixStream::connect(&daemon_sock).is_ok();
    AttachInputs {
        shell,
        cwd,
        term,
        cols,
        rows,
        hub_active: std::env::var_os("HUB_ACTIVE").is_some(),
        relay_path: paths::locate_sibling("hub-relay"),
        daemon_sock,
        daemon_up,
    }
}

fn exec_shell(shell: &str) -> ! {
    // exec only returns on failure; keep degrading until something runs.
    let _ = std::process::Command::new(shell).exec();
    let _ = std::process::Command::new("/bin/sh").exec();
    std::process::exit(127);
}

pub fn run_attach() -> ! {
    let i = gather_inputs();
    match plan_attach(&i) {
        AttachAction::ExecShell(sh) => exec_shell(&sh),
        AttachAction::ExecRelay { relay, args, env } => {
            let mut cmd = std::process::Command::new(&relay);
            cmd.args(&args);
            for (k, v) in &env {
                cmd.env(k, v);
            }
            let err = cmd.exec(); // returns only on failure
            eprintln!("hub: could not exec relay ({err}); falling back to shell");
            exec_shell(&i.shell)
        }
    }
}
```

⚠️ executor: verify on macOS + Linux. `TIOCGWINSZ` + `winsize` field names are the same on both, but `libc::winsize` on macOS uses `ws_col`/`ws_row` (identical here). Confirm `std::os::unix::process::CommandExt::exec` is imported. Test attach only under a throwaway `$HOME`/pty (the rc gate in Task 10 exercises the real exec path with a fake relay).

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hub-cli --test attach_failsafe`
Expected: PASS (4 tests).

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p hub-cli --test attach_failsafe && cargo build -p hub-cli`
Expected: 4 tests pass; binary builds. No commit.

---

### Task 3: RC snippet design + guards (cross-shell)

Author the exact guarded shell blocks and expose them + their marker constants to Rust. Snippets live as real `.sh` files under `hub/install/` (so an executor can `shellcheck` them) and are `include_str!`'d.

**Files:**
- Create: `hub/install/zsh-snippet.sh`
- Create: `hub/install/bash-snippet.sh`
- Create: `hub/install/bash-profile-bridge.sh`
- Modify: `hub/crates/hub-cli/src/snippet.rs`
- Test: `hub/crates/hub-cli/src/snippet.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `snippet::{BEGIN, END, BRIDGE_BEGIN, BRIDGE_END}: &str` (marker lines).
  - `snippet::{ZSH, BASH, BASH_PROFILE_BRIDGE}: &str` (full block text incl. markers).
  - `snippet::contains_block(content: &str, begin: &str) -> bool`.
  - `snippet::remove_block(content: &str, begin: &str, end: &str) -> String`.

**The exact zsh snippet** (`hub/install/zsh-snippet.sh`):

```sh
# >>> hub shell integration >>>
# Managed by hub. Do not edit this block. Remove with: hub uninstall
if [ -z "${HUB_ACTIVE:-}" ] && [ -z "${HUB_DISABLE:-}" ] && [ -t 1 ] && command -v hub >/dev/null 2>&1; then
  case "$-" in
    *i*) hub attach --new || true ;;
  esac
fi
# <<< hub shell integration <<<
```

**The exact bash snippet** (`hub/install/bash-snippet.sh`) — intentionally byte-identical POSIX-safe logic so the same guards hold in both shells:

```sh
# >>> hub shell integration >>>
# Managed by hub. Do not edit this block. Remove with: hub uninstall
if [ -z "${HUB_ACTIVE:-}" ] && [ -z "${HUB_DISABLE:-}" ] && [ -t 1 ] && command -v hub >/dev/null 2>&1; then
  case "$-" in
    *i*) hub attach --new || true ;;
  esac
fi
# <<< hub shell integration <<<
```

**The bash login → rc bridge** (`hub/install/bash-profile-bridge.sh`):

```sh
# >>> hub bash_profile bridge >>>
# Managed by hub: ensures interactive login shells load ~/.bashrc (where hub lives).
if [ -f "$HOME/.bashrc" ]; then . "$HOME/.bashrc"; fi
# <<< hub bash_profile bridge <<<
```

**Guard rationale (why each clause exists):**
- `[ -z "${HUB_ACTIVE:-}" ]` — **re-exec guard**: inside a hub session the relay sets `HUB_ACTIVE=1`, so the shell it spawns does NOT recurse into another `hub attach --new`.
- `[ -z "${HUB_DISABLE:-}" ]` — **escape hatch**: `HUB_DISABLE=1` bypasses hub entirely (spec §13).
- `[ -t 1 ]` **and** `case "$-" in *i*)` — **non-interactive guard**: skip when stdout is not a tty and when the shell is not interactive (`sh -c`, scripts). Both checks, because a tty-less interactive shell and an interactive-flagged non-tty are both possible.
- `command -v hub` — **stale-snippet guard**: if the `hub` binary is gone (leftover snippet after a botched uninstall), do nothing.
- `hub attach --new || true` — **fail-safe**: `hub attach --new` `exec`s the relay on success and never returns; on any failure-before-exec it returns non-zero, `|| true` swallows it, and the already-running interactive shell continues normally.

⚠️ executor: **do NOT** write `exec hub attach --new` in the rc. A failed `exec` of a missing/broken `hub` binary terminates the shell in both bash and zsh — a locked-out terminal. Plain `hub attach --new || true` is mandatory; the internal `exec` happens *inside* `hub`, after the reachability probe.

- [ ] **Step 1: Write the failing test**

Add to `hub/crates/hub-cli/src/snippet.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_start_and_end_with_declared_markers() {
        assert!(ZSH.trim_start().starts_with(BEGIN));
        assert!(ZSH.trim_end().ends_with(END));
        assert!(BASH.trim_start().starts_with(BEGIN));
        assert!(BASH_PROFILE_BRIDGE.trim_start().starts_with(BRIDGE_BEGIN));
    }

    #[test]
    fn blocks_carry_all_five_guards() {
        for block in [ZSH, BASH] {
            assert!(block.contains("HUB_ACTIVE"), "re-exec guard");
            assert!(block.contains("HUB_DISABLE"), "bypass guard");
            assert!(block.contains("[ -t 1 ]"), "tty guard");
            assert!(block.contains("*i*"), "interactive guard");
            assert!(block.contains("command -v hub"), "stale-binary guard");
            assert!(block.contains("hub attach --new || true"), "fail-safe call");
            assert!(!block.contains("exec hub attach"), "must NOT exec in rc");
        }
    }

    #[test]
    fn remove_block_is_exact_inverse_of_append() {
        let original = "line one\nline two\n";
        let injected = format!("{original}\n{ZSH}\n");
        let restored = remove_block(&injected, BEGIN, END);
        // Removing the block leaves the pre-existing content (trailing whitespace trimmed to original).
        assert!(!contains_block(&restored, BEGIN));
        assert!(restored.starts_with("line one\nline two"));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli snippet::`
Expected: FAIL — `ZSH`, `BEGIN`, `remove_block`, etc. undefined.

- [ ] **Step 3: Create the three `.sh` files** exactly as shown above, then implement `snippet.rs`:

```rust
pub const BEGIN: &str = "# >>> hub shell integration >>>";
pub const END: &str = "# <<< hub shell integration <<<";
pub const BRIDGE_BEGIN: &str = "# >>> hub bash_profile bridge >>>";
pub const BRIDGE_END: &str = "# <<< hub bash_profile bridge <<<";

pub const ZSH: &str = include_str!("../../../install/zsh-snippet.sh");
pub const BASH: &str = include_str!("../../../install/bash-snippet.sh");
pub const BASH_PROFILE_BRIDGE: &str = include_str!("../../../install/bash-profile-bridge.sh");

pub fn contains_block(content: &str, begin: &str) -> bool {
    content.lines().any(|l| l.trim_end() == begin)
}

/// Remove the inclusive `begin..=end` marked block (and one trailing blank line
/// if present). Idempotent; leaves all other lines untouched.
pub fn remove_block(content: &str, begin: &str, end: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in content.lines() {
        let t = line.trim_end();
        if !skipping && t == begin {
            skipping = true;
            // Drop a single preceding blank separator line if we added one.
            if out.last().map(|l| l.is_empty()).unwrap_or(false) {
                out.pop();
            }
            continue;
        }
        if skipping {
            if t == end {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    let mut s = out.join("\n");
    if content.ends_with('\n') && !s.is_empty() {
        s.push('\n');
    }
    s
}
```

⚠️ executor: the `include_str!` paths are relative to `snippet.rs` (`src/`), so `../../../install/` resolves to `hub/install/`. Confirm the workspace lives at `hub/` — if the crate is nested differently, fix the relative depth.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hub-cli snippet::`
Expected: PASS (3 tests).

- [ ] **Step 5: (optional) lint the shell blocks**

Run: `shellcheck -s sh hub/install/*.sh || echo "shellcheck not installed; skip"`
Expected: no errors (or a clean skip).

- [ ] **Step 6: Checkpoint**

Run: `cargo test -p hub-cli snippet::`
Expected: 3 tests pass. No commit.

---

### Task 4: RC file selection incl. bash `.bashrc` / `.bash_profile` split

Pure logic that decides which files to touch per shell. The bash login-file split is the classic footgun; get it exactly right.

**Files:**
- Modify: `hub/crates/hub-cli/src/rcfile.rs`
- Test: `hub/crates/hub-cli/tests/rcfile_split.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `rcfile::Shell::{Zsh, Bash}`.
  - `rcfile::BridgeKind::{AppendSourceBashrc, CreateProfile}`.
  - `rcfile::RcPlan { primary: PathBuf, bridge: Option<(PathBuf, BridgeKind)> }`.
  - `rcfile::plan_rc(shell, home, exists: &dyn Fn(&Path)->bool, sources_bashrc: &dyn Fn(&Path)->bool) -> RcPlan` (pure — filesystem injected).

**Selection rules:**
- **zsh:** primary = `~/.zshrc` (read by every interactive zsh, login or not). No bridge.
- **bash:** primary = `~/.bashrc` (holds the hub snippet). Interactive **login** bash does NOT read `~/.bashrc`, so we bridge:
  - If `~/.bash_profile` exists → target it. If it already sources `.bashrc` → no bridge; else `AppendSourceBashrc` into `~/.bash_profile`.
  - Else if `~/.bash_login` exists → same logic against `~/.bash_login`.
  - Else (neither exists) → `CreateProfile` at `~/.bash_profile` (sources `~/.profile` if present, then `~/.bashrc`). We must create `~/.bash_profile` because once it exists bash stops reading `~/.profile`, so the created file preserves `~/.profile` behavior.

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/rcfile_split.rs`:

```rust
use hub_cli::rcfile::{plan_rc, BridgeKind, Shell};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

fn fs(existing: &[&str], sources: &[&str]) -> (impl Fn(&Path) -> bool, impl Fn(&Path) -> bool) {
    let ex: HashSet<PathBuf> = existing.iter().map(PathBuf::from).collect();
    let sr: HashSet<PathBuf> = sources.iter().map(PathBuf::from).collect();
    (move |p: &Path| ex.contains(p), move |p: &Path| sr.contains(p))
}

#[test]
fn zsh_targets_zshrc_no_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Zsh, home, &e, &s);
    assert_eq!(plan.primary, PathBuf::from("/h/.zshrc"));
    assert!(plan.bridge.is_none());
}

#[test]
fn bash_primary_is_bashrc() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(plan.primary, PathBuf::from("/h/.bashrc"));
}

#[test]
fn bash_profile_exists_and_sources_bashrc_needs_no_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_profile"], &["/h/.bash_profile"]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert!(plan.bridge.is_none());
}

#[test]
fn bash_profile_exists_without_sourcing_gets_append_bridge() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_profile"], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_profile"), BridgeKind::AppendSourceBashrc))
    );
}

#[test]
fn bash_no_login_file_creates_bash_profile() {
    let home = Path::new("/h");
    let (e, s) = fs(&[], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_profile"), BridgeKind::CreateProfile))
    );
}

#[test]
fn bash_login_used_when_no_profile() {
    let home = Path::new("/h");
    let (e, s) = fs(&["/h/.bash_login"], &[]);
    let plan = plan_rc(Shell::Bash, home, &e, &s);
    assert_eq!(
        plan.bridge,
        Some((PathBuf::from("/h/.bash_login"), BridgeKind::AppendSourceBashrc))
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli --test rcfile_split`
Expected: FAIL — `plan_rc` unimplemented.

- [ ] **Step 3: Implement `rcfile.rs`**

```rust
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shell {
    Zsh,
    Bash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeKind {
    /// Append a `. ~/.bashrc` block to an existing login file.
    AppendSourceBashrc,
    /// Create ~/.bash_profile from scratch (source ~/.profile then ~/.bashrc).
    CreateProfile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcPlan {
    pub primary: PathBuf,
    pub bridge: Option<(PathBuf, BridgeKind)>,
}

pub fn plan_rc(
    shell: Shell,
    home: &Path,
    exists: &dyn Fn(&Path) -> bool,
    sources_bashrc: &dyn Fn(&Path) -> bool,
) -> RcPlan {
    match shell {
        Shell::Zsh => RcPlan {
            primary: home.join(".zshrc"),
            bridge: None,
        },
        Shell::Bash => {
            let primary = home.join(".bashrc");
            let profile = home.join(".bash_profile");
            let login = home.join(".bash_login");
            let bridge = if exists(&profile) {
                if sources_bashrc(&profile) {
                    None
                } else {
                    Some((profile, BridgeKind::AppendSourceBashrc))
                }
            } else if exists(&login) {
                if sources_bashrc(&login) {
                    None
                } else {
                    Some((login, BridgeKind::AppendSourceBashrc))
                }
            } else {
                Some((profile, BridgeKind::CreateProfile))
            };
            RcPlan { primary, bridge }
        }
    }
}
```

Add `pub mod rcfile;` to `lib.rs` (already listed in Task 2 Step 1).

⚠️ executor: verify on macOS (Terminal.app opens **login** shells → `.bash_profile` path is hit) and Linux (most terminals open **non-login** interactive shells → `.bashrc` path). Both must end up running the snippet.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hub-cli --test rcfile_split`
Expected: PASS (6 tests).

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p hub-cli --test rcfile_split`
Expected: 6 tests pass. No commit.

---

### Task 5: `hub install` — dirs, backup, inject, idempotent, manifest

Wires Tasks 3+4 into the real (temp-HOME-testable) install: create `~/.hub` (0700), back up each rc file before first touch, inject the marked block idempotently, and record everything in a manifest for exact uninstall.

**Files:**
- Modify: `hub/crates/hub-cli/src/manifest.rs`
- Modify: `hub/crates/hub-cli/src/install.rs`
- Test: `hub/crates/hub-cli/tests/install_idempotent.rs`

**Interfaces:**
- Consumes: `snippet::{ZSH, BASH, BASH_PROFILE_BRIDGE, BEGIN, contains_block}`; `rcfile::{plan_rc, Shell, BridgeKind, RcPlan}`; `paths::*`.
- Produces:
  - `manifest::{Manifest, TouchedFile, AutostartEntry}`, `manifest::{load(&Path) -> Manifest, save(&Path, &Manifest)}`.
  - `install::detect_shells(env_shell: Option<&str>, exists: &dyn Fn(&Path)->bool) -> Vec<Shell>` — which shells to install for.
  - `install::create_hub_tree(home: &Path) -> anyhow::Result<()>` (0700 dirs).
  - `install::inject_all(home, shells, &mut Manifest) -> anyhow::Result<()>` — idempotent injection, records `TouchedFile`s.
  - `install::run(home: &Path, yes: bool) -> anyhow::Result<()>` — consent gate + orchestration (calls `autostart::install_autostart` from Task 6).

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/install_idempotent.rs`:

```rust
use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::Manifest;
use hub_cli::rcfile::Shell;
use hub_cli::snippet::BEGIN;
use std::fs;

fn count_marker(s: &str) -> usize {
    s.lines().filter(|l| l.trim_end() == BEGIN).count()
}

#[test]
fn install_is_idempotent_and_backs_up() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();
    let after_first = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(count_marker(&after_first), 1, "one block after first install");
    assert!(after_first.contains("export FOO=bar"), "preserves prior content");

    // Backup captured original.
    let entry = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let backup = fs::read_to_string(entry.backup.as_ref().unwrap()).unwrap();
    assert_eq!(backup, "export FOO=bar\n");

    // Second install: no double-inject.
    let mut m2 = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m2).unwrap();
    let after_second = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(count_marker(&after_second), 1, "still exactly one block");
}

#[test]
fn hub_tree_created_0700() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    create_hub_tree(tmp.path()).unwrap();
    let mode = fs::metadata(tmp.path().join(".hub")).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli --test install_idempotent`
Expected: FAIL — `create_hub_tree`/`inject_all` unimplemented.

- [ ] **Step 3: Implement `manifest.rs`**

```rust
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TouchedFile {
    pub path: String,
    /// Backup of the pre-install content; None if hub created the file.
    pub backup: Option<String>,
    /// True if hub created this file (uninstall deletes it entirely).
    pub created_by_hub: bool,
    /// sha256 of the file as install left it (used to detect later user edits).
    pub post_install_sha256: String,
    /// "snippet" | "bridge" — which marked block hub added.
    pub block: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AutostartEntry {
    Launchd { plist: String, label: String },
    Systemd { unit: String, name: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<TouchedFile>,
    #[serde(default)]
    pub autostart: Option<AutostartEntry>,
    #[serde(default)]
    pub binaries: Vec<String>,
    #[serde(default)]
    pub install_prefix: Option<String>,
}

fn one() -> u32 { 1 }

pub fn load(path: &Path) -> anyhow::Result<Manifest> {
    if !path.exists() {
        return Ok(Manifest { version: 1, ..Default::default() });
    }
    let s = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&s)?)
}

pub fn save(path: &Path, m: &Manifest) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(m)?)?;
    Ok(())
}
```

- [ ] **Step 4: Implement `install.rs`**

```rust
use crate::manifest::{Manifest, TouchedFile};
use crate::rcfile::{plan_rc, BridgeKind, Shell};
use crate::{autostart, manifest, paths, snippet};
use anyhow::Context;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

pub fn detect_shells(env_shell: Option<&str>, exists: &dyn Fn(&Path) -> bool) -> Vec<Shell> {
    // Install for whichever v1 shells the user plausibly has.
    let home = paths::home_dir();
    let mut out = Vec::new();
    let zsh = env_shell.map(|s| s.contains("zsh")).unwrap_or(false)
        || exists(&home.join(".zshrc"));
    let bash = env_shell.map(|s| s.contains("bash")).unwrap_or(false)
        || exists(&home.join(".bashrc"))
        || exists(&home.join(".bash_profile"));
    if zsh {
        out.push(Shell::Zsh);
    }
    if bash {
        out.push(Shell::Bash);
    }
    if out.is_empty() {
        // Default to zsh on macOS-like defaults; still safe (creates ~/.zshrc).
        out.push(Shell::Zsh);
    }
    out
}

pub fn create_hub_tree(home: &Path) -> anyhow::Result<()> {
    for dir in [
        paths::hub_dir(home),
        paths::sessions_dir(home),
        paths::logs_dir(home),
        paths::backups_dir(home),
    ] {
        fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    }
    fs::set_permissions(paths::hub_dir(home), fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn backup_file(home: &Path, file: &Path) -> anyhow::Result<String> {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let base = file.file_name().and_then(|s| s.to_str()).unwrap_or("rcfile");
    let dst = paths::backups_dir(home).join(format!("{base}.{ts}.bak"));
    fs::copy(file, &dst)?;
    Ok(dst.display().to_string())
}

/// Append a marked block (with a single blank separator) if not already present.
/// Returns Some(TouchedFile) if it changed anything, None if already present.
fn ensure_block(
    home: &Path,
    file: &Path,
    begin: &str,
    block: &str,
    block_name: &str,
    create_if_missing_content: Option<&str>,
) -> anyhow::Result<Option<TouchedFile>> {
    let existed = file.exists();
    let current = if existed { fs::read_to_string(file)? } else { String::new() };

    if snippet::contains_block(&current, begin) {
        return Ok(None); // idempotent: no double-inject.
    }

    let backup = if existed {
        Some(backup_file(home, file)?)
    } else {
        None
    };

    let mut new_content = String::new();
    if let (false, Some(seed)) = (existed, create_if_missing_content) {
        new_content.push_str(seed);
    } else {
        new_content.push_str(&current);
    }
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push('\n');
    new_content.push_str(block.trim_end());
    new_content.push('\n');

    let mut f = fs::File::create(file)?;
    f.write_all(new_content.as_bytes())?;

    Ok(Some(TouchedFile {
        path: file.display().to_string(),
        backup,
        created_by_hub: !existed,
        post_install_sha256: sha256_hex(new_content.as_bytes()),
        block: block_name.to_string(),
    }))
}

pub fn inject_all(home: &Path, shells: &[Shell], m: &mut Manifest) -> anyhow::Result<()> {
    let exists = |p: &Path| p.exists();
    let sources_bashrc = |p: &Path| {
        fs::read_to_string(p)
            .map(|c| c.contains("bashrc"))
            .unwrap_or(false)
    };

    for &shell in shells {
        let plan = plan_rc(shell, home, &exists, &sources_bashrc);
        let block = match shell {
            Shell::Zsh => snippet::ZSH,
            Shell::Bash => snippet::BASH,
        };
        if let Some(t) = ensure_block(home, &plan.primary, snippet::BEGIN, block, "snippet", None)? {
            m.entries.push(t);
        }
        if let Some((bridge_file, kind)) = plan.bridge {
            let seed = match kind {
                BridgeKind::CreateProfile => Some(
                    "# created by hub: preserve ~/.profile, then load ~/.bashrc\n\
                     if [ -f \"$HOME/.profile\" ]; then . \"$HOME/.profile\"; fi\n",
                ),
                BridgeKind::AppendSourceBashrc => None,
            };
            if let Some(t) = ensure_block(
                home,
                &bridge_file,
                snippet::BRIDGE_BEGIN,
                snippet::BASH_PROFILE_BRIDGE,
                "bridge",
                seed,
            )? {
                m.entries.push(t);
            }
        }
    }
    Ok(())
}

pub fn run(home: &Path, yes: bool) -> anyhow::Result<()> {
    let env_shell = std::env::var("SHELL").ok();
    let exists = |p: &Path| p.exists();
    let shells = detect_shells(env_shell.as_deref(), &exists);

    if !yes {
        println!("hub install will:");
        println!("  - create {} (0700)", paths::hub_dir(home).display());
        for s in &shells {
            println!("  - inject a guarded snippet for {s:?} (backing up your rc first)");
        }
        println!("  - set up daemon autostart ({})", autostart::kind_label());
        print!("Proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            anyhow::bail!("aborted by user");
        }
    }

    create_hub_tree(home)?;
    let mut m = manifest::load(&paths::manifest_path(home))?;
    inject_all(home, &shells, &mut m)?;

    // Autostart (Task 6). daemon binary sits beside `hub` (assumption A5).
    if let Some(daemon) = paths::locate_sibling("hub-daemon") {
        match autostart::install_autostart(&daemon, home) {
            Ok(entry) => m.autostart = Some(entry),
            Err(e) => eprintln!("hub: autostart setup skipped: {e:#}"),
        }
    } else {
        eprintln!("hub: hub-daemon not found next to `hub`; autostart skipped");
    }

    manifest::save(&paths::manifest_path(home), &m)?;
    println!("hub installed. Open a new terminal to start capturing sessions.");
    println!("Bypass anytime with HUB_DISABLE=1; remove with `hub uninstall`.");
    Ok(())
}
```

Add `pub mod install; pub mod manifest;` to `lib.rs` (already listed). Add `impl std::fmt::Debug`/`Display` is derived. Note `autostart::kind_label()` and `autostart::install_autostart` are stubbed in Task 1 and implemented in Task 6; add temporary stubs now:

```rust
// autostart.rs (temporary until Task 6)
use crate::manifest::AutostartEntry;
use std::path::Path;
pub fn kind_label() -> &'static str { if cfg!(target_os = "macos") { "launchd" } else { "systemd --user" } }
pub fn install_autostart(_daemon: &Path, _home: &Path) -> anyhow::Result<AutostartEntry> {
    anyhow::bail!("autostart not yet implemented")
}
```

⚠️ executor: run every install test under `tempfile::tempdir()` as `$HOME` — never against your real home. The tests already do this.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test -p hub-cli --test install_idempotent`
Expected: PASS (2 tests).

- [ ] **Step 6: Checkpoint**

Run: `cargo test -p hub-cli --test install_idempotent`
Expected: 2 tests pass. No commit.

---

### Task 6: Daemon autostart (launchd / systemd)

File-content generation is pure and unit-tested; the actual `launchctl`/`systemctl` calls are isolated and flagged for on-OS verification.

**Files:**
- Modify: `hub/crates/hub-cli/src/autostart.rs`
- Test: `hub/crates/hub-cli/tests/autostart_content.rs`

**Interfaces:**
- Consumes: `manifest::AutostartEntry`; Plan 2 seam **A5** (`hub-daemon` foreground binary).
- Produces:
  - `autostart::LAUNCHD_LABEL: &str = "com.hub.daemon"`, `autostart::SYSTEMD_NAME: &str = "hub-daemon.service"`.
  - `autostart::launchd_plist(program: &Path) -> String`.
  - `autostart::systemd_unit(program: &Path) -> String`.
  - `autostart::install_autostart(daemon: &Path, home: &Path) -> anyhow::Result<AutostartEntry>`.
  - `autostart::remove_autostart(entry: &AutostartEntry) -> anyhow::Result<()>`.
  - `autostart::kind_label() -> &'static str`.

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/autostart_content.rs`:

```rust
use hub_cli::autostart::{launchd_plist, systemd_unit, LAUNCHD_LABEL, SYSTEMD_NAME};
use std::path::Path;

#[test]
fn launchd_plist_has_label_program_and_runatload() {
    let p = launchd_plist(Path::new("/opt/hub/hub-daemon"));
    assert!(p.contains(LAUNCHD_LABEL));
    assert!(p.contains("<string>/opt/hub/hub-daemon</string>"));
    assert!(p.contains("<key>RunAtLoad</key>"));
    assert!(p.contains("<key>KeepAlive</key>"));
    assert!(p.trim_start().starts_with("<?xml"));
}

#[test]
fn systemd_unit_has_execstart_restart_and_wantedby() {
    let u = systemd_unit(Path::new("/opt/hub/hub-daemon"));
    assert!(u.contains("ExecStart=/opt/hub/hub-daemon"));
    assert!(u.contains("Restart=on-failure"));
    assert!(u.contains("WantedBy=default.target"));
    assert_eq!(SYSTEMD_NAME, "hub-daemon.service");
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli --test autostart_content`
Expected: FAIL — functions unimplemented.

- [ ] **Step 3: Implement `autostart.rs`**

```rust
use crate::manifest::AutostartEntry;
use std::path::Path;
use std::process::Command;

pub const LAUNCHD_LABEL: &str = "com.hub.daemon";
pub const SYSTEMD_NAME: &str = "hub-daemon.service";

pub fn kind_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "launchd"
    } else {
        "systemd --user"
    }
}

pub fn launchd_plist(program: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{prog}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>ProcessType</key>
  <string>Background</string>
</dict>
</plist>
"#,
        label = LAUNCHD_LABEL,
        prog = program.display()
    )
}

pub fn systemd_unit(program: &Path) -> String {
    format!(
        "[Unit]\n\
         Description=Terminal Hub daemon (router/registry)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={prog}\n\
         Restart=on-failure\n\
         RestartSec=2\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        prog = program.display()
    )
}

#[cfg(target_os = "macos")]
pub fn install_autostart(daemon: &Path, home: &Path) -> anyhow::Result<AutostartEntry> {
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let plist_path = dir.join(format!("{LAUNCHD_LABEL}.plist"));
    std::fs::write(&plist_path, launchd_plist(daemon))?;
    // Modern bootstrap; ignore "already bootstrapped" errors.
    let uid = unsafe { libc::getuid() };
    let _ = Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{uid}"), &plist_path.display().to_string()])
        .status();
    Ok(AutostartEntry::Launchd {
        plist: plist_path.display().to_string(),
        label: LAUNCHD_LABEL.to_string(),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn install_autostart(daemon: &Path, home: &Path) -> anyhow::Result<AutostartEntry> {
    let dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&dir)?;
    let unit_path = dir.join(SYSTEMD_NAME);
    std::fs::write(&unit_path, systemd_unit(daemon))?;
    let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
    let _ = Command::new("systemctl")
        .args(["--user", "enable", "--now", SYSTEMD_NAME])
        .status();
    Ok(AutostartEntry::Systemd {
        unit: unit_path.display().to_string(),
        name: SYSTEMD_NAME.to_string(),
    })
}

pub fn remove_autostart(entry: &AutostartEntry) -> anyhow::Result<()> {
    match entry {
        AutostartEntry::Launchd { plist, label } => {
            let uid = unsafe { libc::getuid() };
            let _ = Command::new("launchctl")
                .args(["bootout", &format!("gui/{uid}/{label}")])
                .status();
            let _ = std::fs::remove_file(plist);
        }
        AutostartEntry::Systemd { unit, name } => {
            let _ = Command::new("systemctl")
                .args(["--user", "disable", "--now", name])
                .status();
            let _ = std::fs::remove_file(unit);
            let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).status();
        }
    }
    Ok(())
}
```

Add `pub mod autostart;` to `lib.rs`.

⚠️ executor: verify on macOS + Linux. On macOS confirm `launchctl bootstrap gui/$UID <plist>` loads (older systems: `launchctl load -w <plist>`; `bootout` vs `unload` must match). On Linux confirm `systemctl --user` works in your session (needs a user D-Bus / lingering enabled for headless). Never run these against a machine you don't want a real LaunchAgent/unit on — use a throwaway account or comment out the `Command` calls when testing content only.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hub-cli --test autostart_content`
Expected: PASS (2 tests).

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p hub-cli --test autostart_content && cargo build -p hub-cli`
Expected: 2 tests pass; builds on this OS. No commit.

---

### Task 7: `hub uninstall` — restore, surgical fallback, dry-run

Full clean per spec §14. Byte-for-byte restore when the file is untouched since install (satisfies the gate); surgical block-removal when the user edited it (never clobber their edits).

**Files:**
- Modify: `hub/crates/hub-cli/src/uninstall.rs`
- Test: `hub/crates/hub-cli/tests/uninstall_restore.rs`

**Interfaces:**
- Consumes: `manifest::{Manifest, TouchedFile, AutostartEntry, load}`; `snippet::{BEGIN, END, BRIDGE_BEGIN, BRIDGE_END, remove_block}`; `autostart::remove_autostart`; `daemon_client::{list_sessions, shutdown_daemon}` (Task 8); `paths::*`.
- Produces:
  - `uninstall::restore_file(t: &TouchedFile) -> anyhow::Result<RestoreOutcome>` (pure-ish; filesystem).
  - `uninstall::RestoreOutcome::{RestoredBackup, SurgicallyRemoved, Deleted, Missing}`.
  - `uninstall::plan_dry_run(home: &Path, m: &Manifest) -> Vec<String>` (human-readable "will touch" list).
  - `uninstall::run(home: &Path, yes: bool, dry_run: bool) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/uninstall_restore.rs`:

```rust
use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::Manifest;
use hub_cli::rcfile::Shell;
use hub_cli::uninstall::{restore_file, RestoreOutcome};
use std::fs;

#[test]
fn uninstall_restores_zshrc_byte_for_byte() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let original = "export FOO=bar\nalias ll='ls -la'\n";
    fs::write(home.join(".zshrc"), original).unwrap();

    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();
    // File now contains the block.
    assert!(fs::read_to_string(home.join(".zshrc")).unwrap().contains(">>> hub"));

    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::RestoredBackup));

    let restored = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert_eq!(restored, original, "byte-for-byte restore");
}

#[test]
fn user_edited_file_is_surgically_cleaned_not_clobbered() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    fs::write(home.join(".zshrc"), "export FOO=bar\n").unwrap();
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Zsh], &mut m).unwrap();

    // Simulate a post-install user edit.
    let mut c = fs::read_to_string(home.join(".zshrc")).unwrap();
    c.push_str("export ADDED_LATER=1\n");
    fs::write(home.join(".zshrc"), &c).unwrap();

    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::SurgicallyRemoved));

    let cleaned = fs::read_to_string(home.join(".zshrc")).unwrap();
    assert!(!cleaned.contains(">>> hub"), "hub block gone");
    assert!(cleaned.contains("export FOO=bar"), "kept original");
    assert!(cleaned.contains("export ADDED_LATER=1"), "kept user edit");
}

#[test]
fn created_file_is_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // No .bashrc/.bash_profile → install CREATES ~/.bash_profile bridge.
    fs::write(home.join(".bashrc"), "").unwrap(); // primary exists, bridge created
    create_hub_tree(home).unwrap();
    let mut m = Manifest::default();
    inject_all(home, &[Shell::Bash], &mut m).unwrap();

    let created = m.entries.iter().find(|e| e.created_by_hub);
    if let Some(t) = created {
        let path = t.path.clone();
        let outcome = restore_file(t).unwrap();
        assert!(matches!(outcome, RestoreOutcome::Deleted));
        assert!(!std::path::Path::new(&path).exists());
    }
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test -p hub-cli --test uninstall_restore`
Expected: FAIL — `restore_file` unimplemented.

- [ ] **Step 3: Implement `uninstall.rs`**

```rust
use crate::manifest::{Manifest, TouchedFile};
use crate::{autostart, daemon_client, manifest, paths, snippet};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    RestoredBackup,
    SurgicallyRemoved,
    Deleted,
    Missing,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}

fn block_markers(block: &str) -> (&'static str, &'static str) {
    match block {
        "bridge" => (snippet::BRIDGE_BEGIN, snippet::BRIDGE_END),
        _ => (snippet::BEGIN, snippet::END),
    }
}

pub fn restore_file(t: &TouchedFile) -> anyhow::Result<RestoreOutcome> {
    let path = Path::new(&t.path);
    if !path.exists() {
        return Ok(RestoreOutcome::Missing);
    }
    if t.created_by_hub {
        fs::remove_file(path)?;
        return Ok(RestoreOutcome::Deleted);
    }
    let current = fs::read(path)?;
    if sha256_hex(&current) == t.post_install_sha256 {
        // Untouched since install → exact restore from backup.
        if let Some(backup) = &t.backup {
            fs::copy(backup, path)?;
            return Ok(RestoreOutcome::RestoredBackup);
        }
    }
    // User edited it: surgically remove only our marked block.
    let (begin, end) = block_markers(&t.block);
    let cleaned = snippet::remove_block(&String::from_utf8_lossy(&current), begin, end);
    fs::write(path, cleaned)?;
    Ok(RestoreOutcome::SurgicallyRemoved)
}

pub fn plan_dry_run(home: &Path, m: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for t in &m.entries {
        if t.created_by_hub {
            out.push(format!("delete (hub-created): {}", t.path));
        } else if let Some(b) = &t.backup {
            out.push(format!("restore {} from backup {}", t.path, b));
        } else {
            out.push(format!("clean hub block in {}", t.path));
        }
    }
    if let Some(a) = &m.autostart {
        out.push(format!("remove autostart: {a:?}"));
    }
    out.push(format!("delete tree: {}", paths::hub_dir(home).display()));
    for b in &m.binaries {
        out.push(format!("remove binary: {b}"));
    }
    out
}

pub async fn run(home: &Path, yes: bool, dry_run: bool) -> anyhow::Result<()> {
    let m = manifest::load(&paths::manifest_path(home))?;

    // Warn about live sessions (best-effort; daemon may be down).
    let sock = paths::daemon_sock_path(home);
    let live = daemon_client::list_sessions(&sock).await.unwrap_or_default();

    if dry_run {
        println!("hub uninstall --dry-run (nothing will change):");
        println!("  {} live session(s) would terminate", live.len());
        for line in plan_dry_run(home, &m) {
            println!("  {line}");
        }
        return Ok(());
    }

    if !yes {
        println!("hub uninstall will terminate {} live session(s) and:", live.len());
        for line in plan_dry_run(home, &m) {
            println!("  {line}");
        }
        print!("Proceed? [y/N] ");
        std::io::Write::flush(&mut std::io::stdout())?;
        let mut ans = String::new();
        std::io::stdin().read_line(&mut ans)?;
        if !matches!(ans.trim(), "y" | "Y" | "yes") {
            anyhow::bail!("aborted by user");
        }
    }

    // 1. Stop daemon + relays (daemon kills relays; assumption: shutdown ends them).
    let _ = daemon_client::shutdown_daemon(&sock).await;
    // 2. Restore rc files.
    for t in &m.entries {
        match restore_file(t) {
            Ok(o) => println!("  {}: {o:?}", t.path),
            Err(e) => eprintln!("  {}: restore failed: {e:#}", t.path),
        }
    }
    // 3. Remove autostart.
    if let Some(a) = &m.autostart {
        let _ = autostart::remove_autostart(a);
    }
    // 4. Delete ~/.hub.
    let _ = fs::remove_dir_all(paths::hub_dir(home));
    // 5. Remove binaries (last; unlinking a running binary is fine on unix).
    for b in &m.binaries {
        let _ = fs::remove_file(b);
    }
    println!("hub uninstalled. Open a new terminal for a clean shell.");
    Ok(())
}
```

Add `pub mod uninstall;` to `lib.rs`. `daemon_client::shutdown_daemon` is added in Task 8 (stub it now returning `Ok(())`).

⚠️ executor: verify on macOS + Linux under a throwaway `$HOME`. The byte-for-byte path is what the gate checks; the surgical path protects real users who edited their rc after install. Removing the currently-running `hub` binary (step 5) succeeds on unix (unlink of an open file) but confirm before shipping.

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test -p hub-cli --test uninstall_restore`
Expected: PASS (3 tests).

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p hub-cli --test uninstall_restore`
Expected: 3 tests pass. No commit.

---

### Task 8: `hub status` — daemon client + reconciliation buckets

Talk to the daemon over `hub-transport`, then diff live sessions against on-disk records into healthy / ghost / orphan buckets (spec §9). Client is tested against an in-process fake daemon so it does not depend on Plan 2 being built.

**Files:**
- Modify: `hub/crates/hub-cli/src/daemon_client.rs`
- Modify: `hub/crates/hub-cli/src/reconcile.rs` (new file; add `pub mod reconcile;` to `lib.rs`)
- Modify: `hub/crates/hub-cli/src/status.rs`
- Test: `hub/crates/hub-cli/tests/reconcile.rs`
- Test: `hub/crates/hub-cli/tests/daemon_client_fake.rs`

**Interfaces:**
- Consumes: `hub_proto::{ControlMsg, Frame, SessionInfo, SessionId, Origin, encode_control}`; `hub_transport::{connect, bind_listener, FramedConn}`; Plan 2 seams **A3** (List→Sessions) and **A6** (record files with `sock`).
- Produces:
  - `daemon_client::list_sessions(sock: &Path) -> anyhow::Result<Vec<SessionInfo>>`.
  - `daemon_client::kill_session(sock: &Path, id: SessionId) -> anyhow::Result<()>` (Task 9 uses it).
  - `daemon_client::shutdown_daemon(sock: &Path) -> anyhow::Result<()>` (best-effort; used by uninstall).
  - `reconcile::RecordFile { info: SessionInfo, sock: String }`, `reconcile::scan_records(dir: &Path) -> Vec<RecordFile>`.
  - `reconcile::Buckets { healthy: Vec<SessionInfo>, ghost: Vec<RecordFile>, orphan: Vec<SessionInfo> }`.
  - `reconcile::reconcile(live: &[SessionInfo], records: &[RecordFile], sock_alive: &dyn Fn(&Path)->bool) -> Buckets`.
  - `status::run(home: &Path) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing reconciliation test**

Create `hub/crates/hub-cli/tests/reconcile.rs`:

```rust
use hub_cli::reconcile::{reconcile, RecordFile};
use hub_proto::{Origin, SessionId, SessionInfo};
use std::path::Path;

fn si(id: u64) -> SessionInfo {
    SessionInfo {
        id: SessionId(id),
        origin: Origin::External,
        title: format!("s{id}"),
        pid: 100 + id as u32,
        started_unix: 0,
        cols: 80,
        rows: 24,
    }
}

fn rec(id: u64, sock: &str) -> RecordFile {
    RecordFile { info: si(id), sock: sock.to_string() }
}

#[test]
fn buckets_split_healthy_ghost_orphan() {
    let live = vec![si(1), si(3)]; // daemon sees 1 and 3
    let records = vec![rec(1, "/s/1.sock"), rec(2, "/s/2.sock")]; // disk has 1 and 2
    // socket 2 is dead → ghost; 1 is healthy; 3 live but unrecorded → orphan.
    let alive = |p: &Path| p != Path::new("/s/2.sock");
    let b = reconcile(&live, &records, &alive);

    assert_eq!(b.healthy.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![1]);
    assert_eq!(b.ghost.iter().map(|r| r.info.id.0).collect::<Vec<_>>(), vec![2]);
    assert_eq!(b.orphan.iter().map(|s| s.id.0).collect::<Vec<_>>(), vec![3]);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p hub-cli --test reconcile`
Expected: FAIL — `reconcile` unimplemented.

- [ ] **Step 3: Implement `reconcile.rs`**

```rust
use hub_proto::{SessionId, SessionInfo};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct RecordFile {
    #[serde(flatten)]
    pub info: SessionInfo,
    pub sock: String,
    // record_version tolerated but unused here.
    #[serde(default)]
    pub record_version: u32,
}

pub fn scan_records(dir: &Path) -> Vec<RecordFile> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(s) = std::fs::read_to_string(&p) {
                if let Ok(rec) = serde_json::from_str::<RecordFile>(&s) {
                    out.push(rec);
                }
            }
        }
    }
    out
}

#[derive(Debug, Default)]
pub struct Buckets {
    pub healthy: Vec<SessionInfo>,
    pub ghost: Vec<RecordFile>,
    pub orphan: Vec<SessionInfo>,
}

pub fn reconcile(
    live: &[SessionInfo],
    records: &[RecordFile],
    sock_alive: &dyn Fn(&Path) -> bool,
) -> Buckets {
    let live_ids: HashSet<SessionId> = live.iter().map(|s| s.id).collect();
    let record_ids: HashSet<SessionId> = records.iter().map(|r| r.info.id).collect();

    let mut b = Buckets::default();
    for s in live {
        if record_ids.contains(&s.id) {
            b.healthy.push(s.clone());
        } else {
            b.orphan.push(s.clone()); // live but no record
        }
    }
    for r in records {
        if !live_ids.contains(&r.info.id) && !sock_alive(Path::new(&r.sock)) {
            b.ghost.push(r.clone()); // recorded, daemon doesn't see it, socket dead
        }
    }
    b
}
```

- [ ] **Step 4: Write the failing daemon-client test (fake daemon)**

Create `hub/crates/hub-cli/tests/daemon_client_fake.rs`:

```rust
use hub_cli::daemon_client::list_sessions;
use hub_proto::{ControlMsg, Frame, Origin, SessionId, SessionInfo};
use hub_transport::bind_listener;

#[tokio::test]
async fn list_sessions_roundtrips_against_fake_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hubd.sock");

    let listener = bind_listener(&sock).await.unwrap();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut conn = hub_transport::FramedConn::new(stream);
        // Expect a List frame.
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::List) => {}
            other => panic!("expected List, got {other:?}"),
        }
        let sessions = vec![SessionInfo {
            id: SessionId(7),
            origin: Origin::External,
            title: "vscode".into(),
            pid: 4321,
            started_unix: 1,
            cols: 100,
            rows: 30,
        }];
        conn.write_frame(&hub_proto::encode_control(&ControlMsg::Sessions { sessions }))
            .await
            .unwrap();
    });

    let got = list_sessions(&sock).await.unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id, SessionId(7));
    server.await.unwrap();
}
```

- [ ] **Step 5: Run both to verify they fail**

Run: `cargo test -p hub-cli --test reconcile --test daemon_client_fake`
Expected: FAIL — `list_sessions`/reconcile not yet built (reconcile now passes after Step 3; client fails).

- [ ] **Step 6: Implement `daemon_client.rs` and `status.rs`**

`daemon_client.rs`:

```rust
use anyhow::anyhow;
use hub_proto::{ControlMsg, Frame, SessionId, SessionInfo};
use hub_transport::{connect, FramedConn};
use std::path::Path;

async fn one_shot(sock: &Path, msg: ControlMsg) -> anyhow::Result<Frame> {
    let mut conn: FramedConn = connect(sock).await?;
    conn.write_frame(&hub_proto::encode_control(&msg)).await?;
    conn.read_frame().await
}

pub async fn list_sessions(sock: &Path) -> anyhow::Result<Vec<SessionInfo>> {
    match one_shot(sock, ControlMsg::List).await? {
        Frame::Control(ControlMsg::Sessions { sessions }) => Ok(sessions),
        Frame::Control(ControlMsg::Error { message }) => Err(anyhow!(message)),
        other => Err(anyhow!("unexpected reply to List: {other:?}")),
    }
}

pub async fn kill_session(sock: &Path, id: SessionId) -> anyhow::Result<()> {
    // A4: daemon acks with Closed on success, Error on failure.
    match one_shot(sock, ControlMsg::Kill { id }).await? {
        Frame::Control(ControlMsg::Closed { .. }) => Ok(()),
        Frame::Control(ControlMsg::Error { message }) => Err(anyhow!(message)),
        _ => Ok(()), // best-effort
    }
}

pub async fn shutdown_daemon(sock: &Path) -> anyhow::Result<()> {
    // Best-effort: kill every live session, then let autostart removal stop the daemon.
    if let Ok(sessions) = list_sessions(sock).await {
        for s in sessions {
            let _ = kill_session(sock, s.id).await;
        }
    }
    Ok(())
}
```

`status.rs`:

```rust
use crate::{daemon_client, paths, reconcile};
use std::path::Path;

pub async fn run(home: &Path) -> anyhow::Result<()> {
    let sock = paths::daemon_sock_path(home);
    let live = match daemon_client::list_sessions(&sock).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("hub: daemon unreachable ({e:#}); showing records only");
            Vec::new()
        }
    };
    let records = reconcile::scan_records(&paths::sessions_dir(home));
    let sock_alive = |p: &Path| std::os::unix::net::UnixStream::connect(p).is_ok();
    let b = reconcile::reconcile(&live, &records, &sock_alive);

    println!("HEALTHY ({}):", b.healthy.len());
    for s in &b.healthy {
        println!("  {:>4}  {:?}  {}", s.id.0, s.origin, s.title);
    }
    println!("GHOST (relay crashed, cleanup) ({}):", b.ghost.len());
    for r in &b.ghost {
        println!("  {:>4}  {:?}  {}", r.info.id.0, r.info.origin, r.info.title);
    }
    println!("ORPHAN (live, no record; adopt/kill) ({}):", b.orphan.len());
    for s in &b.orphan {
        println!("  {:>4}  {:?}  {}", s.id.0, s.origin, s.title);
    }
    Ok(())
}
```

Add `pub mod daemon_client; pub mod reconcile; pub mod status;` to `lib.rs`.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `cargo test -p hub-cli --test reconcile --test daemon_client_fake`
Expected: PASS (2 tests total).

- [ ] **Step 8: Checkpoint**

Run: `cargo test -p hub-cli --test reconcile --test daemon_client_fake`
Expected: all pass. No commit.

---

### Task 9: `hub kill <id>`

Thin wrapper over `daemon_client::kill_session`, validated against a fake daemon.

**Files:**
- Modify: `hub/crates/hub-cli/src/kill.rs`
- Test: `hub/crates/hub-cli/tests/kill_fake.rs`

**Interfaces:**
- Consumes: `daemon_client::kill_session`; `paths::daemon_sock_path`; `hub_proto::SessionId`; Plan 2 seam **A4**.
- Produces: `kill::run(home: &Path, id: u64) -> anyhow::Result<()>`.

- [ ] **Step 1: Write the failing test**

Create `hub/crates/hub-cli/tests/kill_fake.rs`:

```rust
use hub_cli::kill;
use hub_proto::{ControlMsg, Frame, SessionId};
use hub_transport::bind_listener;

#[tokio::test]
async fn kill_sends_kill_and_accepts_closed_ack() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("hubd.sock");
    let listener = bind_listener(&sock).await.unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut conn = hub_transport::FramedConn::new(stream);
        match conn.read_frame().await.unwrap() {
            Frame::Control(ControlMsg::Kill { id }) => assert_eq!(id, SessionId(9)),
            other => panic!("expected Kill, got {other:?}"),
        }
        conn.write_frame(&hub_proto::encode_control(&ControlMsg::Closed {
            id: SessionId(9),
            exit_code: Some(0),
        }))
        .await
        .unwrap();
    });

    // kill::run resolves the sock from HOME; point HOME at our tempdir.
    std::env::set_var("HUB_SOCK", &sock);
    kill::run(dir.path(), 9).await.unwrap();
    std::env::remove_var("HUB_SOCK");
    server.await.unwrap();
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p hub-cli --test kill_fake`
Expected: FAIL — `kill::run` still the placeholder.

- [ ] **Step 3: Implement `kill.rs`**

```rust
use crate::{daemon_client, paths};
use hub_proto::SessionId;
use std::path::Path;

pub async fn run(home: &Path, id: u64) -> anyhow::Result<()> {
    let sock = paths::daemon_sock_path(home);
    daemon_client::kill_session(&sock, SessionId(id)).await?;
    println!("killed session {id}");
    Ok(())
}
```

Add `pub mod kill;` to `lib.rs`.

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p hub-cli --test kill_fake`
Expected: PASS.

- [ ] **Step 5: Checkpoint**

Run: `cargo test -p hub-cli --test kill_fake && cargo build -p hub-cli`
Expected: pass; `hub` binary builds with all five subcommands. No commit.

---

### Task 10: RC fail-safe gate suite (drives real bash/zsh in a pty)

The non-negotiable hard gate. Everything runs under a `tempfile` `$HOME`; interactive-shell behavior is exercised by spawning real `bash`/`zsh` inside a `portable-pty` pseudo-terminal (so `[ -t 1 ]` is genuinely true and `$-` contains `i`), with a **fake `hub`** on `PATH` — the real daemon/relay are never involved.

**Files:**
- Create: `hub/crates/hub-cli/tests/common/mod.rs`
- Create: `hub/crates/hub-cli/tests/rc_gate.rs`

**Interfaces:**
- Consumes: `install::{create_hub_tree, inject_all}`; `manifest::Manifest`; `uninstall::restore_file`; `rcfile::Shell`; `snippet::BEGIN`; dev-dep `portable_pty`.
- Produces: test-only harness `common::{TempHome, run_in_pty, ShellProbe}`.

- [ ] **Step 1: Write the pty + fake-hub harness**

Create `hub/crates/hub-cli/tests/common/mod.rs`:

```rust
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A throwaway HOME with a fake `hub` on PATH and a call-log file.
pub struct TempHome {
    pub dir: tempfile::TempDir,
    pub bin: PathBuf,
    pub call_log: PathBuf,
}

impl TempHome {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fakebin");
        std::fs::create_dir_all(&bin).unwrap();
        let call_log = dir.path().join("hub_calls.log");

        // Fake `hub`: logs its args, exits with FAKE_HUB_EXIT (default 1 = daemon down).
        let hub = bin.join("hub");
        std::fs::write(
            &hub,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$HUB_CALL_LOG\"\nexit \"${FAKE_HUB_EXIT:-1}\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hub, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        TempHome { dir, bin, call_log }
    }

    pub fn home(&self) -> &Path {
        self.dir.path()
    }

    pub fn calls(&self) -> String {
        std::fs::read_to_string(&self.call_log).unwrap_or_default()
    }
}

/// Result of driving a shell.
pub struct ShellProbe {
    pub output: String,
}

impl ShellProbe {
    pub fn reached_prompt(&self) -> bool {
        self.output.contains("HUB_TEST_OK")
    }
}

/// Spawn `program args` in a real pty with the given env, write `drive` lines,
/// read until "HUB_TEST_OK" or timeout. A tty is present so `[ -t 1 ]` is true.
pub fn run_in_pty(
    program: &str,
    args: &[&str],
    envs: &[(&str, &str)],
    drive: &[&str],
    timeout: Duration,
) -> ShellProbe {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
        .unwrap();

    let mut cmd = CommandBuilder::new(program);
    for a in args {
        cmd.arg(a);
    }
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    for line in drive {
        writeln!(writer, "{line}").unwrap();
    }
    writer.flush().unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut acc = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    acc.extend_from_slice(&chunk[..n]);
                    let _ = tx.send(acc.clone());
                }
                Err(_) => break,
            }
        }
    });

    let deadline = Instant::now() + timeout;
    let mut out = String::new();
    while Instant::now() < deadline {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(100)) {
            out = String::from_utf8_lossy(&bytes).to_string();
            if out.contains("HUB_TEST_OK") {
                break;
            }
        }
    }
    let _ = child.kill();
    ShellProbe { output: out }
}
```

- [ ] **Step 2: Write the failing gate tests**

Create `hub/crates/hub-cli/tests/rc_gate.rs`:

```rust
mod common;

use common::{run_in_pty, TempHome};
use hub_cli::install::{create_hub_tree, inject_all};
use hub_cli::manifest::Manifest;
use hub_cli::rcfile::Shell;
use hub_cli::snippet::BEGIN;
use hub_cli::uninstall::{restore_file, RestoreOutcome};
use std::time::Duration;

fn install_zsh_snippet(h: &TempHome, original: &str) -> Manifest {
    std::fs::write(h.home().join(".zshrc"), original).unwrap();
    std::fs::write(h.home().join(".bashrc"), original).unwrap();
    create_hub_tree(h.home()).unwrap();
    let mut m = Manifest::default();
    inject_all(h.home(), &[Shell::Zsh, Shell::Bash], &mut m).unwrap();
    m
}

fn bash_env<'a>(h: &'a TempHome, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
    let home = h.home().to_str().unwrap();
    let path = format!("{}:{}", h.bin.display(), std::env::var("PATH").unwrap());
    // Leak to get 'static-ish str lifetimes for the harness signature.
    let path: &'static str = Box::leak(path.into_boxed_str());
    let home: &'static str = Box::leak(home.to_string().into_boxed_str());
    let log: &'static str = Box::leak(h.call_log.display().to_string().into_boxed_str());
    let mut v = vec![("HOME", home), ("PATH", path), ("HUB_CALL_LOG", log)];
    v.extend_from_slice(extra);
    v
}

// GATE 1: daemon down → sourcing rc still yields a working plain shell.
#[test]
fn gate_daemon_down_yields_working_shell() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("FAKE_HUB_EXIT", "1")]); // 1 = daemon unreachable
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "shell must stay usable when daemon is down");
    assert!(
        h.calls().contains("attach --new"),
        "snippet should have attempted attach"
    );
}

// GATE 2: HUB_DISABLE=1 → snippet fully bypassed.
#[test]
fn gate_hub_disable_bypasses_snippet() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("HUB_DISABLE", "1")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt());
    assert!(
        !h.calls().contains("attach --new"),
        "HUB_DISABLE=1 must skip hub entirely"
    );
}

// GATE 3: re-exec guard — HUB_ACTIVE=1 does not recurse.
#[test]
fn gate_hub_active_does_not_recurse() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("HUB_ACTIVE", "1")]);
    let probe = run_in_pty(
        "bash",
        &["--norc", "-i"],
        &env,
        &[
            &format!("source {}/.bashrc", h.home().display()),
            "echo HUB_TEST_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt());
    assert!(
        !h.calls().contains("attach --new"),
        "inside a hub session the snippet must not recurse"
    );
}

// GATE 4: non-interactive (no tty) → snippet bypassed.
#[test]
fn gate_non_interactive_bypasses() {
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    // Plain piped bash (no pty) → `[ -t 1 ]` false.
    let out = std::process::Command::new("bash")
        .args(["--norc", "-c"])
        .arg(format!(
            "source {}/.bashrc; echo HUB_TEST_OK",
            h.home().display()
        ))
        .env("HOME", h.home())
        .env("PATH", format!("{}:{}", h.bin.display(), std::env::var("PATH").unwrap()))
        .env("HUB_CALL_LOG", &h.call_log)
        .env("FAKE_HUB_EXIT", "1")
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stdout).contains("HUB_TEST_OK"));
    assert!(
        !h.calls().contains("attach --new"),
        "non-interactive shells must skip the snippet"
    );
}

// GATE 5: no double-inject — install twice → snippet appears exactly once.
#[test]
fn gate_no_double_inject() {
    let h = TempHome::new();
    let mut m = install_zsh_snippet(&h, "export FOO=bar\n");
    inject_all(h.home(), &[Shell::Zsh], &mut m).unwrap(); // second run
    let content = std::fs::read_to_string(h.home().join(".zshrc")).unwrap();
    let count = content.lines().filter(|l| l.trim_end() == BEGIN).count();
    assert_eq!(count, 1, "exactly one hub block after two installs");
}

// GATE 6: uninstall restores the rc file byte-for-byte.
#[test]
fn gate_uninstall_restores_byte_for_byte() {
    let h = TempHome::new();
    let original = "export FOO=bar\nalias g=git\n";
    let m = install_zsh_snippet(&h, original);
    let t = m.entries.iter().find(|e| e.path.ends_with(".zshrc")).unwrap();
    let outcome = restore_file(t).unwrap();
    assert!(matches!(outcome, RestoreOutcome::RestoredBackup));
    assert_eq!(
        std::fs::read_to_string(h.home().join(".zshrc")).unwrap(),
        original
    );
}
```

- [ ] **Step 3: Run the suite to verify it fails**

Run: `cargo test -p hub-cli --test rc_gate`
Expected: FAIL initially only if any wiring is missing; if Tasks 3/5/7 are done, GATES 5 & 6 pass immediately and GATES 1-4 exercise the shell. If a shell test hangs, the 10s timeout ends it and the assertion fails — investigate before proceeding.

- [ ] **Step 4: Make the suite pass**

No new production code should be required — the gates validate Tasks 3/5/7. If a gate fails:
- GATE 1 failing (shell hung / errored) → the snippet used `exec` or lacks `|| true`; fix `hub/install/*.sh`.
- GATE 2/3 failing → guard clause missing; fix the snippet.
- GATE 4 failing → `[ -t 1 ]` guard missing.
- GATE 5 failing → `ensure_block` idempotency broken (`contains_block`).
- GATE 6 failing → backup not captured before inject, or `restore_file` sha compare wrong.

Run: `cargo test -p hub-cli --test rc_gate`
Expected: PASS (6 tests).

- [ ] **Step 5: Add the zsh variant (skip if zsh absent)**

Append to `rc_gate.rs`:

```rust
#[test]
fn gate_daemon_down_yields_working_shell_zsh() {
    if std::process::Command::new("zsh").arg("--version").output().is_err() {
        eprintln!("zsh not installed; skipping");
        return;
    }
    let h = TempHome::new();
    install_zsh_snippet(&h, "export FOO=bar\n");
    let env = bash_env(&h, &[("FAKE_HUB_EXIT", "1"), ("ZDOTDIR", h.home().to_str().unwrap())]);
    let probe = run_in_pty(
        "zsh",
        &["-i"],
        &env,
        &[
            &format!("source {}/.zshrc", h.home().display()),
            "echo HUB_TEST_OK",
            "exit",
        ],
        Duration::from_secs(10),
    );
    assert!(probe.reached_prompt(), "zsh must stay usable when daemon is down");
    assert!(h.calls().contains("attach --new"));
}
```

Run: `cargo test -p hub-cli --test rc_gate`
Expected: PASS (7 tests; the zsh one may print "skipping" on Linux CI without zsh).

⚠️ executor: verify on macOS + Linux. macOS default shell is zsh; Linux CI often lacks zsh (the skip handles it). `ZDOTDIR` points zsh at the temp HOME so it reads our test `.zshrc`. If `bash --norc -i` under a pty is slow to reach a prompt, raise the timeout — never remove the `|| true` to "fix" a hang.

- [ ] **Step 6: Checkpoint (full gate)**

Run: `cargo test -p hub-cli`
Expected: every hub-cli test passes, including the 7 rc-gate tests. State: "rc fail-safe gate GREEN — daemon-down shell works, HUB_DISABLE bypass, HUB_ACTIVE no-recurse, non-interactive skip, no double-inject, byte-for-byte restore." No commit.

---

## Self-Review

Applied against the spec (§13 fail-safe rc injection, §14 uninstall) and the task's SCOPE.

**1. Spec coverage**
- §13 skip if `HUB_ACTIVE` → snippet guard + GATE 3 + `plan_attach` (Task 2). ✅
- §13 skip if non-interactive (`[ -t 1 ]` / `$-`) → snippet + GATE 4. ✅
- §13 `HUB_DISABLE=1` bypass → snippet + GATE 2. ✅
- §13 daemon unreachable → fall through to plain shell → `plan_attach` daemon_up check (Task 2) + `hub attach --new || true` + GATE 1. ✅
- §13 backs up rc; correct file per shell (zsh `.zshrc`; bash `.bashrc`/`.bash_profile` split); no double-inject → Tasks 4, 5 + GATE 5. ✅
- §14 warn "N live sessions" → `uninstall::run` (Task 7). ✅
- §14 stop daemon + kill relays → `daemon_client::shutdown_daemon` (Task 8) called by Task 7. ✅ (depends on A4/shutdown seam — flagged.)
- §14 restore rc from backup exactly → `restore_file` RestoredBackup + GATE 6. ✅
- §14 remove autostart → `autostart::remove_autostart` (Task 6). ✅
- §14 delete `~/.hub` → Task 7 step 4. ✅
- §14 remove binaries → Task 7 step 5 (from `manifest.binaries`). ✅ (see gap note below)
- §14 `--dry-run` → `plan_dry_run` + Task 7. ✅
- SCOPE `hub status` incl. origin + ghost/orphan buckets → Task 8. ✅
- SCOPE `hub kill <id>` → Task 9. ✅
- SCOPE autostart launchd/systemd → Task 6. ✅
- `~/.hub` 0700 → `create_hub_tree` + test. ✅
- clap "4" added to workspace deps + noted → Global Constraints + Task 1. ✅

**Gap found & fixed inline:** the manifest starts with an empty `binaries` list and `install::run` never populates it, so §14 "remove binaries" would be a no-op. **Fix (apply in Task 5 `install::run`, before `manifest::save`):**

```rust
// Record installed binaries + prefix so uninstall can remove them (spec §14).
for name in ["hub", "hub-daemon", "hub-relay", "hub-tui"] {
    if let Some(p) = paths::locate_sibling(name) {
        m.binaries.push(p.display().to_string());
    }
}
m.install_prefix = std::env::current_exe()
    .ok()
    .and_then(|p| p.parent().map(|d| d.display().to_string()));
```
(`hub` itself is located relative to `current_exe`; on the running binary `locate_sibling("hub")` finds it since it sits in the same dir.) ⚠️ executor: removing the in-use `hub` binary is a deliberate unlink-while-running (fine on unix). Guard behind a printed warning; consider deferring `hub`'s own removal to a tiny detached `rm` if you observe issues on a given filesystem.

**2. Placeholder scan** — no "TBD"/"add error handling"/"similar to Task N" placeholders; every code step shows complete, compilable code and every shell block is literal. The `env_shell` default in `detect_shells` and the daemon `shutdown` are concrete, not deferred. ✅

**3. Type consistency**
- `AttachInputs`/`AttachAction`/`plan_attach` — identical names in Task 2 definition, test, and `run_attach`. ✅
- `RcPlan { primary, bridge: Option<(PathBuf, BridgeKind)> }` — matches Task 4 test and Task 5 consumer. ✅
- `TouchedFile { path, backup, created_by_hub, post_install_sha256, block }` — written in Task 5, read identically in Task 7 `restore_file`. ✅
- `snippet::{BEGIN, END, BRIDGE_BEGIN, BRIDGE_END, remove_block, contains_block}` — consistent across Tasks 3/5/7. ✅
- `RecordFile { info, sock }` + `reconcile(...)` — Task 8 definition matches its test; `Buckets { healthy, ghost, orphan }` consistent. ✅
- `daemon_client::{list_sessions, kill_session, shutdown_daemon}` — signatures match callers in status/kill/uninstall. ✅
- `hub_proto` usage (`ControlMsg::List/Sessions/Kill/Closed`, `SessionInfo`, `SessionId`) matches the frozen contract. ✅
- **Inconsistency found & fixed:** Task 2 Step 1 adds a `[lib]` target and `lib.rs`; Task 1's `main.rs` originally declared `mod ...;`. Resolved by Task 2 Step 1's instruction to switch `main.rs` to `use hub_cli::...;` and to grow `lib.rs`'s `pub mod` list as each module lands. `reconcile` is listed in `lib.rs` from Task 2 but only created in Task 8 — **fix:** in Task 2 Step 1, comment out `pub mod reconcile;` (and any not-yet-created module) and uncomment it in the task that creates the file, so the crate always compiles. Applied as a note here; executor should keep `lib.rs` in sync with existing files at each task boundary.

All issues found were fixed inline above.

---

## Execution Handoff

**Plan complete and saved to `docs/superpowers/plans/2026-07-19-plan-3-install.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
