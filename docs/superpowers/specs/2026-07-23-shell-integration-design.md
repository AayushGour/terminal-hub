# Shell Integration (OSC 7 / OSC 133) — Design Spec

## 1. Purpose

Foundation for two user-facing features (this spec covers ONLY the signal
source; consumers are separate specs):

- Live cwd in the terminal titlebar (`TileFrame.svelte`) and sidebar row
  (`SessionList.svelte`), truncated to `.../parent1/parent2/current` — updates
  the instant the shell's cwd changes (`cd`), not on a poll.
- A live "command finished, with exit code" signal, feeding the (separately
  spec'd) notification feature.

## 2. Why OSC escape codes, not OS-level polling

The relay already runs every byte of pty OUTPUT through `hub-term::Screen`
(a `vt100` parser) for rendering/replay. Shells can be told to emit standard
"shell integration" escape sequences on every prompt/command boundary — OSC 7
for cwd, OSC 133 for command lifecycle + exit code (the same mechanism VSCode,
iTerm2, kitty, and WezTerm use). This is event-driven (fires the instant it
happens, no polling latency) and needs no OS-specific process introspection
(no macOS `libproc` FFI, no Linux `/proc` reads). The cost is a second,
lightweight tokenizing pass over pty output bytes the relay already has in
hand — see §7 (Performance) for why this must NOT be a second full VT100
parse.

**Requires shell cooperation**: only fires if the running shell sources the
hook (see §4). Falls back to silence (stale/empty cwd, no exit-code signal)
for shells that don't — acceptable, matches this project's existing
fail-safe philosophy (a broken/missing hook must never break the shell
itself).

## 3. Wire formats (frozen contract — shell hook emits exactly this, relay parses exactly this)

Both BEL-terminated (`\x07`), not ST-terminated — simpler single-byte
terminator, matches the common convention.

- **OSC 7 (cwd):** `ESC ] 7 ; file://<host><path> BEL`
  e.g. `\x1b]7;file://myhost/Users/aayush/projects/terminal-hub\x07`
  Parsing rule: after `file://`, find the FIRST `/` — everything from that
  `/` onward (inclusive) is the absolute path. Ignore the hostname entirely
  (don't validate/match it).
- **OSC 133 (command lifecycle):**
  - `ESC ] 133 ; A BEL` — prompt displayed (new prompt cycle started).
  - `ESC ] 133 ; C BEL` — a command just started executing.
  - `ESC ] 133 ; D ; <exit_code> BEL` — the command finished, `<exit_code>` is
    an ASCII decimal integer (e.g. `0`, `1`, `127`).
  This spec's parser only ACTS on `D;<exit_code>` (bump `activity_seq`, store
  `last_exit_code`). `A` and `C` must be recognized and silently consumed (so
  they never leak as literal text into the screen/replay) but need no
  handler logic beyond that — kept in the wire format for forward
  compatibility with future features (e.g. a "command currently running"
  indicator), not used yet. Do not build unused logic around them now.

## 4. Shell hook (rc injection)

Added to the SAME managed block `hub` already injects (between
`>>> hub shell integration >>>` / `<<<` markers) in `hub/install/zsh-snippet.sh`
and `hub/install/bash-snippet.sh` — NOT a second block. `hub-cli`'s
`snippet.rs` (`crates/hub-cli/src/snippet.rs`) pulls these files in via
`include_str!` at compile time, so editing the `.sh` files is sufficient; no
Rust-side duplication to update.

**Critical, non-obvious detail:** the existing block's guard is
`[ -z "${HUB_ACTIVE:-}" ]` — it only fires in the OUTER, uncaptured login
shell (which immediately `exec`s into `hub attach --new` and exits). The
actual shell a user interacts with is the INNER shell `hub-relay` spawns as
the pty child, which has `HUB_ACTIVE=1` set in its environment (see
`hub-relay/src/relay.rs`'s `shell_env` — "a relay-spawned shell is BY
DEFINITION a hub-managed shell"). So the new hook must be guarded by the
OPPOSITE condition, `[ -n "${HUB_ACTIVE:-}" ]` — it only runs INSIDE the
captured inner shell, where cwd/commands actually matter.

zsh (uses `precmd_functions`/`preexec_functions` arrays — append, don't
overwrite, in case a user's own `.zshrc` already defines these):

```sh
if [ -n "${HUB_ACTIVE:-}" ] && [ -n "${ZSH_VERSION:-}" ]; then
  typeset -g __hub_cmd_running=0
  __hub_preexec() { __hub_cmd_running=1; printf '\033]133;C\007'; }
  __hub_precmd() {
    local ec=$?
    if [ "$__hub_cmd_running" = 1 ]; then
      printf '\033]133;D;%s\007' "$ec"
      __hub_cmd_running=0
    fi
    printf '\033]7;file://%s%s\007' "$HOST" "$PWD"
    printf '\033]133;A\007'
  }
  precmd_functions+=(__hub_precmd)
  preexec_functions+=(__hub_preexec)
fi
```

bash (no native precmd/preexec — `PROMPT_COMMAND` + `trap DEBUG`; must guard
against the DEBUG trap firing for `PROMPT_COMMAND`'s own internals, and must
capture `$?` as the FIRST statement in the precmd function, before it):

```sh
if [ -n "${HUB_ACTIVE:-}" ] && [ -n "${BASH_VERSION:-}" ]; then
  __hub_cmd_running=0
  __hub_preexec() {
    [ -n "${COMP_LINE:-}" ] && return
    [ "$BASH_COMMAND" = "$PROMPT_COMMAND" ] && return
    if [ "$__hub_cmd_running" = 0 ]; then
      __hub_cmd_running=1
      printf '\033]133;C\007'
    fi
  }
  trap '__hub_preexec' DEBUG
  __hub_precmd() {
    local ec=$?
    if [ "$__hub_cmd_running" = 1 ]; then
      printf '\033]133;D;%s\007' "$ec"
      __hub_cmd_running=0
    fi
    printf '\033]7;file://%s%s\007' "$HOSTNAME" "$PWD"
    printf '\033]133;A\007'
  }
  PROMPT_COMMAND="__hub_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
```

Both must be validated during implementation against real interactive shells
(nested subshells, `set -e`, custom `PS1`/`precmd` already defined by the
user's own dotfiles) — flagged as a real testing risk, not assumed correct
by construction.

## 5. Backend: parsing + propagation

- **New scanner** (`hub-term` or a new small module in `hub-relay` —
  implementer's call, but it must NOT live inside `Screen`/modify the vendored
  `vt100` crate): a minimal `vte::Perform` implementation that only handles
  `osc_dispatch` for codes `7` and `133`, ignoring everything else (print,
  execute, csi, esc all no-ops). Fed the exact same bytes as
  `Screen::feed` in the relay's pty-output path (`RelayEvent::Output` handler
  in `hub-relay/src/relay.rs`) — NOT stripped from the forwarded stream (OSC
  sequences are already invisible/no-op to `vt100` and to `xterm.js` on the
  frontend, so there's no need to filter them out like the stdin
  focus-report fix did — this is the OUTPUT direction, not input).
  Exposes something like:
  ```rust
  pub enum ShellEvent { Cwd(String), CommandFinished(i32) }
  pub struct ShellIntegration { /* vte::Parser + small Perform state */ }
  impl ShellIntegration {
      pub fn feed(&mut self, bytes: &[u8]) -> Vec<ShellEvent>;
  }
  ```
- **`hub-proto` (`crates/hub-proto/src/types.rs`)**: add to `SessionInfo`:
  `cwd: String` (empty until first OSC 7 seen), `last_exit_code: Option<i32>`,
  `activity_seq: u64` (starts at 0, incremented on every `CommandFinished`).
  Mirror the same 3 fields onto `hub_relay::record::SessionRecord` (so ghost
  records stay consistent) and the frontend-side duplicate struct
  `app/src-tauri/src/reconcile.rs::GhostRecord` (see that file's own comment
  about why it's a structural duplicate, not a dependency).
- **New `ControlMsg` variant** (relay → daemon, over the relay's existing
  persistent control connection — the same one `Opened`/`Closed` already use,
  no new connection/socket needed):
  ```rust
  SessionActivity { id: SessionId, cwd: String, last_exit_code: Option<i32>, activity_seq: u64 },
  ```
  Sent whenever the `ShellIntegration` scanner reports a new event. The relay
  also rewrites its own `SessionRecord` on disk at the same time (atomic
  write, existing `write_atomic` pattern) so a crashed relay's ghost record
  still shows its last-known cwd/exit code.
- **`hub-daemon`**: `Registry` gets an `update_activity(id, cwd, last_exit_code, activity_seq)`
  that mutates the in-memory `SessionInfo` for that session (same map
  `Open`/`Opened` populate — see `registry.rs`). `server.rs` routes the new
  `ControlMsg::SessionActivity` frame arriving on a relay's connection to it,
  the same way it already routes other relay→daemon control messages.

This is why a live push is necessary rather than just rewriting the on-disk
record: the GUI's "healthy" bucket (`app/src-tauri/src/reconcile.rs::bucketize`)
sources its `SessionInfo` from the daemon's live in-memory registry
(`ControlMsg::List`), not from disk — confirmed when this was traced for the
earlier cwd-only design. Disk records only matter for the ghost bucket.

## 6. Frontend

- `app/src/lib/api.ts`: add `cwd: string`, `lastExitCode: number | null`,
  `activitySeq: number` to the `SessionInfo` TS interface (match whatever
  casing `serde` produces — check existing fields' casing convention in this
  file, e.g. `started_unix` is already snake_case, so likely no rename
  needed).
- `app/src/lib/mock.ts`: extend the fake session objects with the same 3
  fields so `VITE_MOCK=1` dev/testing still works without a real backend.
- New pure util (e.g. `app/src/lib/path.ts`): `truncatePath(cwd: string): string`
  — last 3 path segments joined by `/`, prefixed with `.../` only if there
  were MORE segments above those 3 (a path with ≤3 segments total shows as-is,
  no ellipsis). No `~` substitution — literal segments only.
- `TileFrame.svelte`: its `.titlebar` (currently
  `<span class="tt">{origin || "session"} #{id}</span>`, ~26px single row)
  gets the truncated cwd appended in that same row — keep the existing
  single-row/26px layout and visual language, don't redesign the titlebar;
  let the new cwd span ellipsize on overflow like `SessionList.svelte`'s
  `.title` already does (`overflow:hidden; text-overflow:ellipsis; white-space:nowrap`).
- `SessionList.svelte`: its row (currently `{s.title} #{s.id}` at the
  `<button class="title">`) gets the truncated cwd shown too — alongside or
  in place of `s.title` (title is often just the shell name, e.g. "zsh",
  which is less identifying than cwd — implementer's call on exact
  presentation, but cwd must be visible).

## 7. Performance (hard constraints, not suggestions)

- **No new polling timer, anywhere.** This whole design is event-driven off
  bytes already flowing through the relay. If an implementation reaches for
  `tokio::time::interval`/`setInterval` to check cwd or exit code, that's the
  wrong design — stop and re-read §2.
- **The OSC scanner must be a lightweight tokenize-only pass** (`vte::Parser`
  + minimal `Perform`), not a second `vt100::Parser`/second full terminal
  grid. It should cost roughly what one incremental tokenization of the byte
  stream costs — proportional to actual output volume, ~0 for idle sessions,
  and nowhere near double the cost of the existing `Screen::feed` (which
  additionally maintains the full grid/scrollback, the expensive part we are
  NOT duplicating).
- **Frontend updates must be diff-based**, not whole-list re-render: only the
  specific session(s) whose `activity_seq`/`cwd` actually changed should
  cause their row/tile to update.

## 8. Testing

- Unit tests for the OSC scanner: feed synthetic byte sequences (including
  ones split across multiple `feed()` calls, since `vte::Parser` is
  incremental and pty output can arrive in arbitrary chunks) and assert the
  right `ShellEvent`s come out, including a case with each of OSC 0/2 (title,
  already parsed by `vt100` — must keep working unaffected), OSC 7, and OSC
  133 A/C/D interleaved with plain text.
- Shell-hook tests: this project already spawns real `zsh`/`bash` processes
  to test the injected rc snippet (see `crates/hub-cli/tests/rc_gate.rs`) —
  follow that existing pattern: source the snippet with `HUB_ACTIVE=1` set,
  run a command, and assert the exact OSC byte sequences appear on stdout.
- End-to-end: a real interactive session (manual verification, per this
  project's existing pattern for SIGWINCH/focus-reporting) confirming `cd`
  updates the titlebar/sidebar within one reconcile cycle, and a failing
  command shows the right exit code.

## 9. Out of scope (separate spec)

Everything about HOW `activity_seq`/`last_exit_code` changes are surfaced as
notifications (tile border flash, canvas-edge flash, sidebar row flash, OS
notification, color-by-exit-code, click-to-focus) is a separate,
not-yet-written spec that consumes the `SessionInfo` fields this spec adds.
Do not build any of that here.
