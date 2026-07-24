# Project context — <project>

The single source of truth for WHAT + WHY. business-analyst seeds it; architect adds design; anyone appends decisions.

## Goal
<problem + why it matters>

## Users
<who uses this>

## User stories
- As <user>, I want <action>, so <benefit>.
  - AC: <testable>
  - AC: <testable>

## Business rules
<rules, edge cases>

## Constraints
<deadlines, tech limits, must-use / must-avoid>

## Assumptions / open questions
<flagged by business-analyst when the client didn't specify>

## Out of scope
<explicit non-goals>

---

## Architecture  (architect)
- Stack: <...>
- Modules: <module → responsibility>
- Data model: <entities, relationships>
- APIs: <endpoint → purpose → in/out>

## Team  (architect — from the team-formation self-review)
- Core roles in play: <which of the 10 this project uses>
- Specialists added: <name → why the 10 didn't cover it → when added>  (none if standard team suffices)

## Decisions  (append-only; only if it constrains future work)
### D1 — <title>  (<date>, <agent>)
Why: <reason>. Alt: <rejected> because <...>. Impact: <what it locks>. Files: <paths>

### D2 — `hub update` restarts the daemon by direct detached spawn, not by re-triggering launchd/systemd alone  (2026-07-23, senior-dev)
Why: `hub update` must never disrupt a live session. Relays are independent, SPOF-surviving processes and `hub-daemon`'s own startup (`server::run`, before it binds the control socket) re-adopts every still-alive relay from `~/.hub/sessions/*.json` — this is the same mechanism a bare daemon crash/restart already relies on. So the safe update sequence is: `daemon_client::shutdown_daemon` (stops only the process, never sessions) → bounded poll for the socket to stop answering → `setsid()`-detached direct spawn of the new `hub-daemon` binary → bounded poll for the socket to answer again. `install_autostart` is re-run afterward only to keep future logins pointed at the (possibly freshly-written) binary; it is not relied on for the immediate restart because `HUB_SKIP_SERVICE_ACTIVATION` (test harnesses) and CLI-only/no-autostart installs would otherwise never actually relaunch the daemon. If launchd's own `KeepAlive` also races to relaunch, the daemon's existing singleton flock (`hub-daemon/src/singleton.rs`) makes that safe — the loser just exits.
Alt: rely solely on re-running `install_autostart`/launchctl to bounce the daemon — rejected because it depends on activation being enabled and live (not true in tests, and not guaranteed for CLI-only installs), and gives no way to bound/observe when the daemon is actually back up.
Impact: `hub update` (and any future daemon-restart tooling) should keep using `daemon_client::shutdown_daemon` + a direct detached spawn + socket polling as the restart primitive, not invent a second mechanism.
Files: `hub/crates/hub-cli/src/update.rs`, `hub/crates/hub-cli/src/cli.rs`, `hub/crates/hub-cli/src/main.rs`, `hub/crates/hub-cli/src/lib.rs`, `hub/crates/hub-cli/tests/update_preserves_sessions.rs`
