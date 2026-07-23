# Frontend smoke tests

`npx playwright test` runs against a `VITE_MOCK=1` Vite build whose in-memory IPC
(`src/lib/mock.ts`) echoes input→output and returns one mock session. These prove
the xterm.js + Tauri-event wiring without a live daemon. Real backend coverage is
the Rust suite (`cargo test -p hub-app`) plus the manual checklist below.

## Running

```bash
cd hub/app
npx playwright test          # headless, ~2-3s, spins up its own VITE_MOCK=1 dev server
```

`playwright.config.ts` owns the `webServer` (`VITE_MOCK=1 npm run dev` on
`http://localhost:5173`) — no separate server needs to be started first.

## What's covered here vs. elsewhere

| Spec | Proves |
|---|---|
| `echo.spec.ts` | typed keystrokes round-trip through xterm.js and the mock IPC |
| `sessions.spec.ts` | session list row: origin badge, Detach + Kill controls present |
| `grid.spec.ts` | opening a session renders a tile with a terminal and CSS `resize: both` |
| `newsession.spec.ts` | "+ New session" button spawns (mock) and the new session appears after refresh |
| `settings.spec.ts` | buffer-size input shows the persisted value + RAM-tradeoff hint text |
| `startup.spec.ts` | on load (no clicks) Healthy/Orphan/Ghost buckets populate, and Kill/Clean-up remove a row |

None of these touch a real `hub-daemon`/`hub-relay` process — that's deliberate
(fast, deterministic, no pty/socket flakiness in CI). Real-backend proof lives in:

- `hub/app/src-tauri/tests/real_daemon.rs` — drives the ACTUAL daemon + relay
  binaries through `ConnManager` (attach, per-tile isolation, kill vs. detach).
  Runs as part of `cargo test -p hub-app`.
- `hub/app/MANUAL-VERIFICATION.md` — the GUI-visual / real-pty checks a headless
  mock can't cover (rendering, focus-follows-size against a real shell, drag-resize,
  daemon-crash survival). Run this by hand before shipping a GUI change.
