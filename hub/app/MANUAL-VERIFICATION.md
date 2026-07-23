# Manual verification checklist — real daemon + relay

Spec §17: the GUI's acceptance gate is "manual + a few Playwright smokes." The
Playwright suite (`hub/app/tests/`, see `tests/README.md`) runs headless against
a mocked IPC and proves the xterm.js + Tauri-event wiring. It **cannot** prove
anything that needs a real pty, a real daemon process, or human eyes on the
rendered window — visual rendering, `stty`/`tput` reflow against a live shell,
two independent real shells, or daemon-crash survival. This document is that
remaining check. It is run by a human, not CI.

Each item below is one explicit action and one exact, checkable expected result
— not "verify it works." Record a ✅/❌ + note per item in the results log at the
bottom.

## Prerequisites

1. Plans 1-3 built and installed (`hub-daemon`, `hub-relay`, `hub` CLI on `PATH`,
   `~/.hub` set up). From the workspace root:
   ```bash
   cd hub && cargo build -p hub-daemon -p hub-relay -p hub-cli
   ```
2. The daemon running and reachable at `~/.hub/hubd.sock` (or `$HUB_DIR/hubd.sock`
   if `HUB_DIR` is set) — `hub status` should succeed without error.
3. **At least one external shell already captured** before the GUI is launched,
   so the "startup discovery" item has something to find. Open a plain terminal
   whose shell rc has the injected snippet (from `hub install`), or manually run:
   ```bash
   hub attach --new
   ```
   in a terminal window you'll keep open and type into during the checklist
   (this is the "original external terminal" referenced below).
4. Build and launch the GUI against the real backend (no `VITE_MOCK`):
   ```bash
   cd hub/app && npm run tauri dev
   ```

## Checklist

1. **Startup discovery.** Look at the left panel within ~1s of the window
   opening, before clicking anything.
   Expected: the **Healthy** section lists a row for the shell opened in
   prerequisite 3, showing its title and `#<id>`, with a blue **External**
   badge (`.badge.ext`, `#58f` background) to its left. No click was needed.

2. **Attach + REPLAY.** Click that session's title text.
   Expected: a bordered tile appears in the grid on the right and **immediately**
   shows the shell's last prompt/output (its current screen) — not a blank
   black box that then fills in. This is the `hub://replay` snapshot painting
   before any live `hub://output` frames arrive.

3. **Live output mirror.** Switch to the *original* external terminal (the one
   from prerequisite 3) and run `ls`.
   Expected: the same `ls` output (the same filenames, same order) appears in
   the hub tile within a fraction of a second, unprompted.

4. **Live input, two masters one slave.** Click inside the hub tile so it has
   focus, type `echo hi`, press Enter.
   Expected: `hi` is printed **in both** the hub tile and the original external
   terminal — confirming input from the hub is reaching the same pty the
   external terminal is attached to, not a separate shell.

5. **Focus-follows-size.** Click the original external terminal first (so the
   hub tile loses focus), then click back into the hub tile.
   Expected: the tile's border turns green (`border-color: #2d6`, CSS rule
   `.tilewrap:focus-within`) immediately on focus. If the tile's on-screen pixel
   size differs from the external terminal's, the shell reflows: run
   `tput cols` in the external terminal, note the number, then run `tput cols`
   again from inside the hub tile (the same underlying shell) — the value now
   matches the tile's fitted column count, not the number from before focus.

6. **Drag-resize.** With the hub tile focused, drag its bottom-right corner
   (the `resize: both` CSS handle) to make it noticeably wider, and stop.
   Expected: after ~50ms (the debounce in `Terminal.svelte`'s `claimSoon`) the
   terminal re-fits — visibly more columns of blank space / re-wrapped prompt —
   and running `tput cols` in that shell now reports the new, larger width.
   While dragging continuously for a couple of seconds, the terminal does
   **not** visibly stutter/flicker on every pixel of drag (confirms the resize
   is debounced, not fired on every mouse-move event).

7. **New Hub session.** Click **+ New session** in the toolbar.
   Expected: within ~1s (the 400ms poll in `Toolbar.svelte` plus relay
   registration time) a new row appears under **Healthy** with a green
   **Hub** badge (`.badge.hub`, `#2d6` background). Click its title: a tile
   opens and shows a working, empty shell prompt (not an error, not blank
   forever) — type a command and see it execute.

8. **Detach vs. kill (distinct).** On a healthy session's row, click
   **Detach**.
   Expected: the tile for that session closes (removed from the grid), the
   row **stays** in the Healthy list, and the underlying shell is still alive
   — switch to (or re-attach) that external terminal and confirm it still
   accepts input / hasn't exited.
   Now click **Kill** on that same row and confirm the `window.confirm`
   dialog ("Kill session `<id>`? This ends the shell for all viewers.").
   Expected: the row disappears from the list entirely, and the external
   terminal shows its shell exiting (prompt returns to the parent shell, or
   the terminal shows "process exited").

9. **Orphan cleanup.** Spawn a Hub session (step 7 again), then **quit the
   hub app** (close the window) — but do **not** kill the daemon or the
   relay — and relaunch it (`npm run tauri dev` again).
   Expected: on restart, that relay still shows up in the list (in **Healthy**
   if both its live socket and on-disk record are present, or in **Orphan**
   — labelled "(live, no record — kill leftovers)" — if only the live socket
   is present with no record). Click **Kill** on it: the row disappears and
   no shell process is left running (check with `ps` for the relay's pid, or
   confirm `hub status` no longer lists it) — i.e. nothing was leaked.

10. **Buffer setting.** In the Settings panel at the bottom of the sidebar, set
    the scrollback number input to `50000` and click **Save**.
    Expected: a green **"saved ✓"** label appears next to the button
    (auto-hides after ~1.5s). Below the input, the hint text reading
    *"Memory ≈ **buffer × line width × live session count**. The 10k default
    is a few MB per open terminal…"* is visible. Open a *new* tile (attach to
    a different/new session) and produce more than 10,000 lines of output in
    it (e.g. `seq 1 20000`); confirm you can scroll back through all of it —
    further than a tile opened before the setting was raised would allow.

11. **Daemon-crash survival (SPOF sanity, GUI-visible slice).** With a tile
    attached and showing live output, find and kill the `hub-daemon` process
    (e.g. `pkill -f hub-daemon` or `kill <pid>` from `ps`).
    Expected: the shell **inside the tile keeps running** — it does not crash,
    freeze, or show an error immediately (the relay/pty is independent of the
    daemon). Restart the daemon (it re-binds `~/.hub/hubd.sock`), then click
    the session list's refresh (↻) or relaunch the GUI: the previously-attached
    session re-lists (Healthy, since its relay never went down) and re-attaching
    to it works normally. (Full SPOF/reconnect-under-load coverage is Plan 2's
    gate — this step is only the GUI-visible sanity slice of it.)

## Results log

| # | Item | Result | Notes |
|---|---|---|---|
| 1 | Startup discovery | | |
| 2 | Attach + REPLAY | | |
| 3 | Live output mirror | | |
| 4 | Live input (2 masters, 1 slave) | | |
| 5 | Focus-follows-size | | |
| 6 | Drag-resize (debounced) | | |
| 7 | New Hub session | | |
| 8 | Detach vs. kill (distinct) | | |
| 9 | Orphan cleanup | | |
| 10 | Buffer setting | | |
| 11 | Daemon-crash survival | | |

## Known, documented caveats (not failures)

These are pre-existing, deliberate contract notes from earlier tasks — expect
them, don't file them as bugs during this checklist:

- **Buffer setting is new-tiles-only** (Task 10): raising scrollback in Settings
  cannot resize xterm's client-side buffer on tiles already open, nor a running
  relay's fixed-size vt ring. Item 10 above tests this correctly (open a *new*
  tile after Save).
- **`hub://connected` is effectively dead in production** (Task 11): the backend
  uses per-tile connections (Approach A) with no single app-wide "connected"
  event; the session list instead self-refreshes on an interval (~5s) plus once
  on mount. So "Orphan cleanup" (item 9) may take up to ~5s to reflect on a
  relaunch rather than being instantaneous — that is expected, not a bug.
- **`spawn_session` shells out to the `hub` CLI** (Task 9) rather than asking the
  daemon to spawn directly, because no daemon "spawn" message exists in the
  current protocol — the ~1s delay in item 7 accounts for that process launch
  + registration.
