# Terminal Hub — Design Spec

**Date:** 2026-07-19
**Status:** Approved for planning
**Codename:** `hub`

## 1. Purpose

A central hub to view and control terminal sessions. The headline capability: **two live views of the same terminal** — one in the app where the terminal was spawned (VS Code, standalone terminal, etc.), and one in the hub app — both able to see output and type input in real time ("2 masters, 1 slave").

Any interactive shell that starts after a one-time install is automatically captured and becomes controllable from the hub. The user must consent to this at install time; if they decline, the app cannot function.

## 2. Goals & Non-Goals

### Goals (v1)
- Capture **every interactive shell** (manual, IDE, any app) via shell-rc injection.
- Mirror one live terminal into two viewers simultaneously (originating terminal + hub).
- **Zero added latency** on the primary terminal (native feel).
- **No system-wide single point of failure** — a central-process crash must never kill a shell.
- Spawn and manage the user's own sessions from the hub app.
- Cross-platform architecture (mac/linux in v1, Windows seams built in).
- Lightweight footprint + a nice GUI.

### Non-Goals
- **Reboot survival** of sessions (no on-disk scrollback persistence). Deferred.
- **Cross-machine / network** attach. Phase 2.
- **Recording / playback** (asciinema-style). Later phase.
- **Plugins / extension API.** Not planned.
- Capturing non-interactive shells (`sh -c`, scripts) — deliberately out of scope.

## 3. Core Concepts

### 3.1 Two PTYs per captured session
An externally-captured terminal already has an outer pty (VS Code's). We do **not** reuse it as the real shell host. Instead:

```
VS Code pty (outer, becomes a dumb relay)
   └─ hub relay ── owns a NEW inner pty ── real $SHELL runs here
                        ▲
                        └── hub app attaches as a second viewer
```

The **inner** pty (owned by the relay) is the real shell. The outer pty just relays bytes. This is what enables "2 views, 1 shell" — same as running `tmux` inside a terminal.

### 3.2 Relay owns the pty (key architectural decision)
The **relay** process — not any central daemon — owns the pty master fd. Consequences:
- Primary terminal reads/writes its pty **natively → zero added latency**.
- A central-process crash **cannot** kill a shell (it doesn't hold the fd).
- The pty's lifetime is bound to the relay, matching the chosen ephemeral model.

### 3.3 Failure domains (why relay-owns-pty is the SPOF-minimal design)
Someone must hold each pty master fd; whoever holds it, their death closes the pty and ends that shell — irreducible. The only design choice is *one owner for all ptys* vs *one owner per session*:

| Process dies | tmux-style (one owner) | **This design (per-session relay)** |
|---|---|---|
| Central process (server/daemon) | ❌ **all** sessions die | ✅ **zero** sessions die (daemon owns no pty) |
| One session's fd-owner | 1 session | 1 session |

Per-session relay = smallest possible blast radius. A relay dying kills exactly one session — the same as a terminal emulator crashing. There is **no system-wide SPOF**; there are N isolated failure domains.

### 3.4 Daemon owns no pty — control plane only
The daemon is a **router + registry**, deliberately **non-critical** — it can crash and restart freely because it owns nothing that can kill a shell.

**Delegation model:** any pty operation (write input, resize via `TIOCSWINSZ`, read output, kill) is a *message* the daemon routes to the relay; the **relay performs the syscall**. Daemon = control plane; relay = data + pty plane. No fd-passing (`SCM_RIGHTS`) — that would reintroduce daemon-owns-pty and a system-wide SPOF.

**What the daemon earns its place with:** one socket instead of N (one attack surface, less memory), one discovery point (no dir-scan races), central reaping + session table + reconciliation authority + cross-cutting ops. It is optional in principle (see rejected approach B) but valuable in practice at scale.

## 4. Architecture (Hybrid)

```
┌──────────────────────────────┐         ┌──────────────┐
│ Relay (per session)          │         │  Hub app     │
│  • OWNS inner pty + shell     │◄──────► │  (Tauri GUI) │
│  • native primary I/O (0 hop) │  socket │  xterm.js    │
│  • 10k vt buffer + REPLAY     │         │  grid/tiles  │
│  • serves viewers             │         └──────┬───────┘
└──────────────┬───────────────┘                │
               │ registers/routes via           │ connects to
               ▼                                 ▼
        ┌──────────────────────────────────────────┐
        │ Daemon (router/registry, owns NO pty)     │
        │  • session table  • routes hub↔relay      │
        │  • reaps dead relays  • internal logging  │
        │  • ONE socket (1 attack surface)          │
        └──────────────────────────────────────────┘
```

### Why hybrid (vs alternatives considered)
| Dimension | A: daemon-owns-pty | B: relay-only (no daemon) | **C: hybrid (chosen)** |
|---|---|---|---|
| System-wide SPOF | ❌ all shells die | ✅ none | ✅ none (daemon owns no pty) |
| Primary latency | ❌ 1 hop | ✅ 0 hop | ✅ 0 hop |
| Listening sockets | ✅ 1 | ❌ N | ✅ 1 |
| Discovery / cross-cutting ops | ✅ central | ❌ scatter/gather | ✅ central |
| Stale-socket reaping | ✅ n/a | ❌ manual | ✅ daemon-assisted |

## 5. Session Lifetime (per-origin)

Same relay mechanism; teardown policy keyed on an **immutable `origin` flag** set at spawn.

| | **Hub-spawned** | **External** (VS Code / standalone) |
|---|---|---|
| Primary controller | Hub app | Originating terminal |
| Teardown trigger | Explicit kill from app (or shell exit) | Originating terminal closes → dies |
| Survives viewer detach | ✅ | (same terminal) |
| Survives hub app close | ✅ (relay is independent) | n/a |
| Survives daemon crash | ✅ | ✅ |
| Survives reboot | ❌ | ❌ |

This is **process-level** persistence (relay outlives its viewers), **not** disk persistence. No tmux-style reattach-after-reboot.

## 6. Relay Independence (detach)

The relay must be spawned **fully detached from birth** so neither the app nor the daemon is its parent:
- `fork` → child `setsid()` (new session leader) → **double-fork** (cannot reacquire a controlling terminal) → parent exits → child reparented to init/launchd (pid 1).
- Spawner **fires and forgets.** No parent's death can signal the relay. This is what makes "survives app-close AND daemon-crash" actually true.

## 7. Data Flow

### Session creation (external capture)
1. Interactive shell starts → rc snippet runs → `exec hub attach --new`.
2. Relay (detached) opens inner pty, spawns `$SHELL`, registers with daemon (`OPEN{shell,cwd,env,size,term,origin=external}`).
3. Relay natively relays outer-terminal ⇄ inner pty. Daemon notified session exists.

### Hub attaches (2nd viewer)
1. Hub → daemon: `LIST` → sessions.
2. Hub → daemon → relay: `ATTACH{sid}`.
3. Relay → hub: `REPLAY{screen snapshot}` (instant current screen) + a SIGWINCH refresh nudge.
4. Relay streams live `OUTPUT`; hub sends `INPUT`.

### Focus-follows-size
- Whichever viewer has **focus** owns pty size. On focus-gain → `CLAIM_SIZE{cols,rows}` → relay resizes pty.
- Move focus to VS Code → snaps to VS Code size; to a hub tile → snaps to tile size.
- Focus via terminal focus reporting (DECSET 1004); fallback **last-active-wins** if a terminal doesn't emit focus events.
- **Debounced (~50ms)** + "only resize if dims changed" to avoid reflow thrash.

### Teardown
- External: outer-terminal relay pipe closes → relay SIGHUPs shell → session dies → daemon broadcasts `CLOSED`.
- Hub-spawned: only explicit `KILL` or shell exit.

## 8. Protocol

Length-prefixed framed messages over one socket, multiplexed by `session_id`.
- **Control** (JSON or compact binary): `OPEN, OPENED, LIST, ATTACH, REPLAY, RESIZE, CLAIM_SIZE, RELEASE, KILL, CLOSED`.
- **Data** (hot path): `{session_id, len}` header + raw pty bytes (no JSON overhead).
- **Backpressure:** bounded buffers; a slow viewer's frames are dropped/coalesced so the shell + primary never stall.

## 9. Session Registry & Reconciliation

- Each relay atomically writes a **record file** `~/.hub/sessions/<id>.json` (id, origin, pid, sock path, started, title) + drops its socket.
- On app/daemon start, run **two scans**:
  - **Auto-discovery** — which sockets actually answer (live).
  - **Record files** — what should exist.
- **Diff → three buckets shown to user:**
  - Live + recorded → healthy, attach.
  - Recorded but socket dead (**ghost**, relay crashed) → offer cleanup/delete.
  - Live socket, no record (**orphan**) → offer reconnect/adopt/kill.
- Metadata-only on disk (bookkeeping); scrollback is never persisted.
- Relay deletes its own record + socket on clean exit; hub retries a dead socket 3× then prunes.

## 10. Sizing / Buffer

- Scrollback buffer default **10k lines**, **user-configurable in UI** (UI surfaces the RAM tradeoff: buffer × session count).
- REPLAY snapshot built from the relay's headless vt100 grid.
- Caveat: full-screen TUIs (vim/htop) may show a slightly stale first frame on attach; SIGWINCH nudge triggers a repaint.

## 11. Security

- Single daemon socket, `0600`; socket dir `0700`. One attack surface.
- **No data logging** — internal/process logging only (lifecycle, errors, session events). Never pty bytes or env.
- Env passed at `OPEN` may contain secrets → never logged; kept in-process only.
- Per-session auth tokens = phase 2 hardening.

## 12. Cross-Platform

- v1: **mac/linux** (shared unix code).
- Phase 2: **Windows** — `portable-pty` ConPTY, named pipe transport, PowerShell `$PROFILE` injection (no true `exec`). Isolated behind `Transport`/`Pty` traits + `#[cfg]` so Windows slots in without a rewrite.
- Daemon lifecycle: launchd/systemd user service (unix) vs Windows service (phase 2).

## 13. Fail-Safe RC Injection (critical)

Highest-risk component — a bad snippet can lock the user out of every terminal.
- Snippet guards: skip if `$HUB_ACTIVE` set, skip if non-interactive (`[ -t 1 ]`), bypass via `HUB_DISABLE=1`.
- **Daemon unreachable → fall through to plain shell** (never hang).
- Install **backs up** rc files; `hub uninstall` restores exactly.
- Correct file per shell (zsh `.zshrc`; bash `.bashrc`/`.bash_profile` split); no double-inject.
- v1 shells: zsh + bash. fish/others = phase 2.

## 14. Uninstall (full clean)

- Warn: "N live sessions will terminate."
- Stop daemon + kill all relays.
- Restore rc files from backup (remove injected snippet exactly).
- Remove autostart entry (launchd plist / systemd unit).
- Delete `~/.hub/` (sockets, records, config, logs).
- Remove installed binaries.
- `hub uninstall --dry-run` lists everything it will touch first.

## 15. Module Layout (Rust cargo workspace)

```
hub/
├─ docs/                      # all technical documentation (see below)
├─ crates/
│  ├─ hub-proto/     # wire protocol: messages, framing, (de)serialize
│  ├─ hub-pty/       # portable-pty abstraction, spawn/resize/read/write, child-death
│  ├─ hub-transport/ # socket abstraction (UnixListener; trait seam for named-pipe)
│  ├─ hub-term/      # headless vt100 parse + 10k ring scrollback → REPLAY
│  ├─ hub-relay/     # BINARY: per-session relay (owns pty, detaches, serves viewers)
│  ├─ hub-daemon/    # BINARY: router/registry (owns no pty, one socket)
│  ├─ hub-cli/       # BINARY: `hub` install/uninstall/attach --new/status/kill
│  └─ hub-tui/       # headless viewer for testing without GUI
├─ app/              # Tauri: xterm.js grid, tiles, drag-resize, focus events
└─ install/          # rc snippets (zsh/bash) + fail-safe guards
```

### docs/ contents
```
docs/
├─ architecture.md        # hybrid model, planes, failure domains, diagrams
├─ protocol.md            # wire messages, framing, sequence diagrams
├─ pty-and-sizing.md      # pty ownership, focus-follows-size, resize rules
├─ session-lifecycle.md   # per-origin lifetimes, spawn→teardown, detach, reaping
├─ rc-injection.md        # install mechanism, fail-safe guards, per-shell
├─ security.md            # socket perms, no-data-logging, threat model
├─ cross-platform.md      # unix now, Windows phase-2 seams
├─ testing.md             # strategy + SPOF/fail-safe gate tests
└─ decisions/             # ADRs: relay-owns-pty, hybrid daemon, Rust+Tauri, ephemeral, focus-sizing
```

## 16. Tech Stack

| Layer | Tech | Why |
|---|---|---|
| Core (pty, daemon, relay) | **Rust** (`portable-pty`, tokio) | cross-platform pty incl ConPTY, tiny static binary, terminal-mux prior art (WezTerm/Zellij/Alacritty) |
| GUI container | **Tauri** | OS webview (no Chromium), MBs not hundreds, Rust-native backend |
| Terminal render | **xterm.js** | the mature web terminal renderer (VS Code uses it) |
| Frontend | Svelte or vanilla TS | compiles light |

## 17. Testing Strategy

| Layer | Approach |
|---|---|
| `hub-proto` | unit: round-trip encode/decode; fuzz framing (partial/split reads) |
| `hub-pty` | integ: spawn `echo` assert output; resize → `stty size`; child-death fires |
| `hub-term` | unit: known vt sequences → grid + REPLAY; ring wraps at buffer cap |
| `hub-transport` | integ: two procs over UDS; perms 0600; slow reader doesn't stall writer |
| relay + daemon | **e2e harness**: daemon + relay (fake shell `cat`) + headless viewer; input→shell, output→viewer, focus-resize snaps pty, relay exit → CLOSED |
| **SPOF gate** | **daemon kill → shell survives → reattach after daemon restart** (non-negotiable) |
| **rc fail-safe gate** | daemon-down → plain shell works; `HUB_DISABLE=1` bypass; no double-inject; uninstall restores exact backup (non-negotiable) |
| GUI | manual + Playwright smoke (xterm.js render/type) |

TDD: proto/term/pty are pure-ish → test-first. Relay/daemon → e2e harness red→green. SPOF-survival + rc fail-safe are **hard gates**.

## 18. MVP Scope

### v1 (must-have)
1. `hub-proto`, `hub-pty`, `hub-transport`, `hub-term`.
2. `hub-relay` + `hub-daemon` — spawn, detach, register, route, mirror, ephemeral teardown.
3. Two viewers, one shell, live (primary native + hub mirror).
4. **Both origins:** external capture + hub-spawned (explicit-kill lifetime).
5. Focus-follows-size w/ debounce.
6. REPLAY on attach (default 10k, configurable) + SIGWINCH refresh.
7. Registry + reconciliation (rediscovery on startup; ghost/orphan buckets; show all shells so user can kill).
8. `hub install/uninstall` — fail-safe rc injection (zsh + bash); full-clean uninstall.
9. Minimal Tauri GUI — session list (origin-labeled), tiled xterm.js, type, drag-resize, detach vs kill actions, buffer-size setting.
10. SPOF-survival + rc fail-safe tests green.

### Deferred
- **Phase 2:** Windows (ConPTY/named pipe/PowerShell); cross-machine attach; fish/other shells; per-session auth tokens; auto-reaper (idle timeout) for hub-spawned orphans.
- **Later:** recording/playback; cross-cutting dashboard (global search, kill-all).
- **Not planned:** plugins; reboot survival.

## 19. Key Risks & Mitigations

| Risk | Mitigation |
|---|---|
| RC injection locks user out | guards + daemon-down fall-through + rc backup + uninstall restore + hard-gate tests |
| Orphaned hub-spawned relays leak | show all shells on startup; manual kill (v1); auto-reaper (phase 2) |
| Relay detach wrong → parent death kills it | setsid + double-fork + reparent to init; test daemon-kill survival |
| `origin` flag mis-set → wrong teardown | set at spawn, immutable, both paths tested |
| Resize reflow thrash on focus bounce | debounce + resize-only-if-changed |
| High-output flood balloons memory / stalls shell | bounded buffers; drop/coalesce for slow viewers |
| Two typers garble line | out of scope (assumed not simultaneous); UI hint later |
| Secrets in env logged | no data logging; env in-process only; 0600 sockets |
