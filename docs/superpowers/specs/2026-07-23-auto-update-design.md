# Auto-update design — Terminal Hub GUI

## Goal

Today, shipping a new version means: bump `hub/npm/term-hub/package.json`, merge to `main`, CI publishes new npm packages — and the user gets nothing until they manually run `npm install -g @term-hub/term-hub` again. There is no in-app update mechanism at all.

This design adds real self-update to the Terminal Hub GUI (`hub-app`, the Tauri desktop app) using the official `tauri-plugin-updater`, so an already-installed GUI detects, downloads, verifies, and installs new versions itself.

## Scope

- **In scope:** macOS + Linux GUI auto-update, end to end (signing, CI artifacts, GitHub Release, in-app check/prompt/install flow).
- **In scope, config-only:** Windows updater configuration (targets, `install_mode`, artifact format) wired into `tauri.conf.json` so enabling the platform later is a small follow-up, not a redesign.
- **Out of scope:** turning on the actual Windows build+publish CI job (tracked separately as part of the Windows rollout). Windows code-signing certificate purchase/provisioning (SmartScreen warnings are accepted for now on Windows once that platform ships). macOS Developer ID signing + notarization (not required for the self-update flow itself; Tauri's automatic ad-hoc signing is sufficient for Apple Silicon to execute the binary).
- **Unaffected:** the CLI tools (`hub`, `hub-daemon`, `hub-relay`) keep shipping exactly as they do today — raw binaries via the npm per-platform optional-dependency packages, installed with `npm install -g @term-hub/term-hub`. Only the GUI's distribution and update path changes.

## Architecture

### Distribution channel unification

Currently `hub-app` ships twice: as a raw binary bundled into the npm platform packages (`@term-hub/darwin-arm64` etc.), launched via a thin hand-rolled `.app`/`.desktop` wrapper that `postinstall.js` creates locally. This design retires that path for the GUI specifically and unifies on one channel: a real, signed Tauri bundle published as a GitHub Release.

- `npm install -g @term-hub/term-hub` still does first-time setup: installs the CLI binaries, and its `postinstall.js` now downloads the current GitHub Release's signed bundle for the host platform and installs it (macOS: unpack `.app.tar.gz` into `~/Applications`; Linux: install the AppImage and register the existing `.desktop` entry to point at it).
- From that point on, npm is out of the loop for the GUI. The running app checks the updater endpoint itself, and all future GUI updates happen in-place via `tauri-plugin-updater` — never touching npm again.
- `hub-app` is removed from the npm per-platform packages' `bin/` staging (smaller npm downloads; no more duplicate copy of the GUI binary living in two places with two version-skew risks).

### Signing

One Ed25519 keypair (generated once via `tauri signer generate`) covers all platforms. Private key + password stored as GitHub Actions secrets (`TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`); public key committed into `tauri.conf.json` under `plugins.updater.pubkey`. Every updater artifact CI produces is signed at build time; the app refuses to install anything whose signature doesn't verify against the embedded pubkey.

### Release pipeline changes

`release.yml` currently builds with `tauri build --no-bundle` deliberately, because distribution was raw-binary-via-npm. This design changes the macOS and Linux build jobs to produce real bundles:

- Set `bundle.createUpdaterArtifacts: true` in `tauri.conf.json`.
- Build real bundles instead of `--no-bundle`: macOS produces `.app` + `.app.tar.gz` + `.sig`; Linux produces AppImage + its update tarball + `.sig`.
- Create (or update) a GitHub Release tagged with the version, uploading the bundle artifacts plus a generated `latest.json` manifest — the endpoint the updater polls. Using `tauri-apps/tauri-action` for this is preferred over hand-rolling the upload + manifest generation, since it builds, signs, creates the release, and emits `latest.json` in one step, matching the existing "only publish if the version is new" idempotency the workflow already has for npm.
- The npm publish job is unchanged except: drop `hub-app` from the files staged into each platform package's `bin/`.

`latest.json` shape (per Tauri v2 updater convention):
```json
{
  "version": "0.2.0",
  "notes": "...",
  "pub_date": "2026-07-23T00:00:00Z",
  "platforms": {
    "darwin-x86_64": { "signature": "...", "url": "https://github.com/AayushGour/terminal-hub/releases/download/v0.2.0/hub-app-x86_64.app.tar.gz" },
    "darwin-aarch64": { "signature": "...", "url": ".../hub-app-aarch64.app.tar.gz" },
    "linux-x86_64": { "signature": "...", "url": ".../hub-app_0.2.0_amd64.AppImage.tar.gz" }
  }
}
```
(Exact key names/format will be confirmed against the installed plugin version's docs during implementation — this is the well-established v2 convention but CI should verify the generated file rather than hand-author it.)

`tauri.conf.json` gets a `plugins.updater` block with `endpoints` pointing at the GitHub Release's `latest.json` (using the `{{target}}`/`{{arch}}`/`{{current_version}}` template variables Tauri substitutes), and the pubkey.

### In-app check/prompt/install flow

- On every launch, the GUI calls the updater plugin's `check()` in the background — no UI while checking.
- Because the app can be left open indefinitely (no forced quit, no system restart), launch alone isn't enough to notice a new release. A background timer also re-runs `check()` once every 24 hours for as long as the app stays open, independent of the launch check.
- If a newer version is available (from either the launch check or a periodic recheck): show a small in-app banner/dialog ("Update available — Restart to install?"). Accepting triggers `downloadAndInstall()`, then the plugin relaunches the app once installed.
- Declining: the session continues unaffected; the check runs again next launch (no persistent nagging, no dismissal state to track).
- Check failures (offline, endpoint unreachable) are silent — logged, not surfaced to the user, retried next launch.
- Signature verification failures are a hard reject — never install, surface as an error state distinct from "no update available."

### Uninstall completeness once the GUI is a real bundle

Investigation of the current code (`hub/app/src-tauri/src/lifecycle.rs`, `hub/npm/term-hub/scripts/postinstall.js` / `preuninstall.js`) found that today's in-app "Uninstall hub & remove app" button and `npm uninstall -g` are almost entirely disjoint:

- The in-app button is a thin pass-through to the `hub` CLI's own `install`/`uninstall` (`hub-cli/src/install.rs`, `uninstall.rs`): reverts the guarded block in `~/.zshrc`/`~/.bashrc`, stops/removes the `hub-daemon` launchd plist or systemd unit, and deletes the entire `~/.hub` tree. As a *second*, separate step, it also self-deletes the running app bundle to the Trash — but only if `current_exe()` resolves to a path whose ancestor genuinely ends in `.app` (`lifecycle.rs`'s `is_safe_app_bundle`). For today's npm installs, the running binary resolves into `node_modules/@term-hub/<platform>/bin/hub-app`, never a real `.app` path, so this self-delete step is a silent no-op.
- `postinstall.js`/`preuninstall.js` only ever create/remove a thin launcher shim (`~/Applications/Terminal Hub.app` on mac, a `.desktop` file on Linux, a `.lnk` on Windows) — they never touch rc files, the daemon, or `~/.hub`.

So today, a full clean removal needs *both* steps, and the in-app button's own "remove app" half quietly does nothing for npm users.

Once D1 lands (postinstall installs the real signed bundle instead of a shim), that asymmetry changes:

- **macOS:** a real `.app` in `~/Applications` needs no separate "registration" step at all — Finder/Launchpad already index anything placed there, so `postinstall.js`'s current hand-rolled `Info.plist`/launcher-script logic (`registerMac()`) is deleted outright, replaced by "extract the downloaded `.app.tar.gz` into `~/Applications`." Because the running exe now genuinely lives under a `.app` bundle, `is_safe_app_bundle` now correctly matches, and "Uninstall hub & remove app" becomes a real, complete, one-click teardown (rc + daemon + `~/.hub` + delete the actual bundle) for npm-installed users too — not just a partial one.
- **Linux:** AppImages don't self-register into the app grid, so a `.desktop` entry is still necessary — but `postinstall.js` now installs the AppImage itself to a **fixed, stable path** (e.g. `~/.local/share/term-hub/hub-app.AppImage`) that the updater overwrites in place on every future update, and points the `.desktop` entry's `Exec=` at that fixed path so it never goes stale across versions. `lifecycle.rs`'s self-delete logic needs a Linux equivalent (delete the AppImage + its `.desktop`/icon entries), since today that path only exists for macOS.
- **`npm uninstall -g` and the in-app button must be fully equivalent** — either path is a complete, standalone teardown, not two halves of one. Concretely, `preuninstall.js` is extended to also perform the rc/daemon/`~/.hub` revert, not just remove the GUI bundle: before removing package files, it checks the same "is hub capture installed?" signal `hub_is_installed` uses (presence of `~/.hub/install-manifest.json` or `~/.hub/bin/hub`); if present, it shells out to `~/.hub/bin/hub uninstall --yes` (the exact same CLI entry point the in-app button calls), then removes the GUI bundle (mac `.app` from `~/Applications`; Linux AppImage + its `.desktop`/icon entries) exactly as the in-app self-delete does. If hub capture was never enabled, that step is skipped (nothing to revert) and only the bundle/CLI-binary removal happens. Either route — clicking "Uninstall hub & remove app" in the GUI, or running `npm uninstall -g @term-hub/term-hub` from a terminal — now ends in the identical state: rc files restored, daemon stopped and its autostart entry removed, `~/.hub` deleted, and the GUI bundle gone.
- Edge case to resolve during implementation planning, not assumed here: if the GUI is currently *running* when `npm uninstall -g` executes, `hub uninstall --yes` already stops the daemon/kills live sessions (shared logic with the in-app path), but the open GUI *window* itself is a separate process `preuninstall.js` doesn't control the way the in-app button's own `app.exit(0)` does. Whether `preuninstall.js` should also try to signal that running window to quit, or simply leave it open as a lame-duck window until the user closes it manually (all its backing files already gone, same as the in-app self-delete-while-running case), is a decision for the implementation plan, not this spec.
- Failure handling matches the existing best-effort philosophy: if `hub uninstall --yes` exits non-zero (corrupted state, permissions issue, etc.), `preuninstall.js` logs a clear message but still proceeds to remove what it can and never fails the overall `npm uninstall` — consistent with `postinstall.js`'s existing `safe()` wrapper pattern.

### Settings UI relocation

Unrelated to auto-update mechanics but bundled into this pass since it directly affects the uninstall button above: `Settings.svelte` (scrollback config + the install/uninstall lifecycle section) is currently mounted inline, always visible, at the bottom of the left sidebar (`App.svelte:104`, directly under `<SessionList>`). This is being moved behind a small gear icon fixed at the foot of the left panel; clicking it opens the existing `Settings.svelte` content in a popover/modal instead of it sitting inline on the main screen. No content or behavior inside `Settings.svelte` changes — only where/how it's revealed. Kept distinct from the update-available prompt (D2): the gear/settings popup is user-initiated configuration, the update prompt is a proactive banner shown on launch — implementers should not conflate the two into one surface.

### Windows readiness (config only)

`plugins.updater` config includes a `windows` section with `installMode: "passive"` (progress bar, no click-through) and NSIS as the target format (simpler pipeline than MSI/WiX, and the existing commented-out Windows CI job already targets `x86_64-pc-windows-msvc` for a standard build, so NSIS fits without extra tooling). No Authenticode signing is configured yet — when the Windows job is turned on, SmartScreen will warn on each new binary hash until a cert (EV recommended for instant reputation) is added; that purchase/setup is explicitly deferred, tracked as follow-up work for the Windows rollout, not blocked by anything in this design.

## Components touched

| Component | Change |
|---|---|
| `hub/app/src-tauri/Cargo.toml` | add `tauri-plugin-updater` |
| `hub/app/package.json` | add `@tauri-apps/plugin-updater` |
| `hub/app/src-tauri/tauri.conf.json` | `bundle.createUpdaterArtifacts: true`, `plugins.updater` block (endpoints, pubkey, windows install_mode) |
| `hub/app/src-tauri/capabilities/default.json` | add updater permission(s) to the `permissions` array |
| `hub/app/src-tauri/src/lib.rs` (or wherever plugins are registered) | register the updater plugin |
| Svelte frontend (new small component) | update-available banner/dialog, calls `check()` on mount, `downloadAndInstall()` on accept |
| `.github/workflows/release.yml` | mac/linux jobs build real signed bundles instead of `--no-bundle`; new step creates GitHub Release + `latest.json` (via `tauri-apps/tauri-action` or manual); npm staging step drops `hub-app` |
| `hub/npm/term-hub/scripts/postinstall.js` | instead of pointing the launcher at a raw `hub-app` binary from an npm optional-dep, download the current GitHub Release bundle for the host platform and install it (mac: extract into `~/Applications`; Linux: install AppImage to a fixed path + write `.desktop` entry pointing at it); keep the existing best-effort `safe()` wrapping (a failed GUI bundle download must never fail `npm install`, same philosophy as today) |
| `hub/npm/term-hub/scripts/preuninstall.js` | extended beyond removing the launcher entry: if hub capture is installed, shell out to `~/.hub/bin/hub uninstall --yes` (same entry point the in-app button uses) before removing the GUI bundle/desktop entry — makes `npm uninstall -g` a complete teardown on its own, matching the in-app button |
| `hub/app/src-tauri/src/lifecycle.rs` | extend the self-delete-to-Trash logic beyond macOS `.app` bundles to cover the Linux AppImage + its `.desktop`/icon entries, so "Uninstall hub & remove app" is a complete teardown on Linux too |
| `hub/npm/packages/*/package.json` | `hub-app` no longer expected/staged in these `bin/` dirs |
| New GitHub secrets | `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` |

## Error handling

- **Update check fails** (network, endpoint down): silent, retry next launch. Never blocks app usage.
- **Download fails or is interrupted**: surfaced as a dismissible error in the update banner ("Couldn't download update — will try again next launch"); app keeps running the current version.
- **Signature verification fails**: hard reject, never installed, logged as an error (this should never happen in practice since CI is the only signer, but it's the backstop against a compromised/corrupted artifact).
- **`postinstall.js` bundle download fails on first install**: same `safe()` best-effort pattern already used for app-menu registration — print a clear message ("run `term-hub` from the CLI; GUI bundle install failed, retry with ...") but never fail the npm install itself.

## Testing

- Unit/manual: bump the version locally, produce two signed builds (old "installed" + new "release"), point the dev build's updater endpoint at a locally-served `latest.json` + artifact, verify check → prompt → download → install → relaunch round-trips and lands on the new version.
- Verify a deliberately mismatched signature is rejected (corrupt the `.sig` or artifact and confirm the plugin refuses to install).
- CI: confirm the release workflow's existing idempotency (skip if version already published) still holds for the new GitHub Release step, not just the npm publish step.
- Manual smoke test of the new `postinstall.js` path on a clean macOS and Linux machine/VM (fresh `npm install -g @term-hub/term-hub` → GUI bundle installed and launches).

## Decisions

### D1 — Unify GUI distribution on signed bundle, drop raw `hub-app` from npm (2026-07-23)
Why: running two parallel distribution shapes for the same binary (raw npm copy vs. updater-managed bundle) creates version-skew and testing surface with no benefit once the updater exists. Alt: keep both in parallel — rejected, adds complexity for no gain once the bundle is the source of truth going forward. Impact: `postinstall.js` now does a network fetch of the GitHub Release bundle at install time (new failure mode, handled via existing best-effort pattern); npm platform packages shrink (CLI binaries only). Files: `hub/npm/term-hub/scripts/postinstall.js`, `hub/npm/packages/*/package.json`, `.github/workflows/release.yml`.

### D2 — Check on launch + every 24h while running, prompt before installing, never fully silent (2026-07-23)
Why: user chose visibility over zero-interaction — a restart is disruptive enough (kills open terminal sessions in the GUI) that it should be the user's call, not automatic. Launch-only checking was later found insufficient: the user pointed out the app can stay open indefinitely without a restart, so a 24h periodic recheck was added to make sure a long-lived session still gets prompted the same day a new version ships, without polling so often it's excessive. Alt: fully silent auto-install — rejected for now, revisit if prompt fatigue becomes a problem. Alt: hourly/6h recheck — rejected as tighter than needed; user picked the lightest-touch option. Impact: update banner/dialog is a required UI component, not optional polish; the frontend needs a persistent timer (e.g. `setInterval`) alongside the on-launch check, both feeding the same banner state.

### D3 — Windows: config-ready, build job deferred, no code-signing cert yet (2026-07-23)
Why: Windows rollout is separate upcoming work; wiring the updater config now (NSIS target, passive install mode) means enabling the platform later doesn't require touching the updater architecture again. SmartScreen warnings from being unsigned are accepted as a known, deferred cost — user has explicitly chosen to bypass rather than buy a cert now. Alt: buy an EV cert now — rejected, out of scope/budget for this pass. Impact: when the Windows CI job is turned on, expect SmartScreen "unknown publisher" warnings on every release until a cert is added; this is expected, not a bug.

### D4 — `npm uninstall -g` and the in-app uninstall button must be equally complete (2026-07-23)
Why: today they're disjoint — the in-app button reverts rc/daemon/`~/.hub` but its "remove app" half silently no-ops for npm installs, while npm's hooks only ever touched a launcher shim. User explicitly required both paths to cleanly and completely uninstall the application, not just look similar. Alt: leave the asymmetry in place (only the in-app button does the full revert) — rejected; user was explicit that both routes must be equivalent, not that one route defers to the other. Impact: `preuninstall.js` gains a new responsibility (shelling out to `~/.hub/bin/hub uninstall --yes` when hub capture is installed) that it never had before; `lifecycle.rs`'s self-delete logic must be extended to Linux (previously macOS-only) so the in-app button is equally complete on both platforms. Files: `hub/npm/term-hub/scripts/preuninstall.js`, `hub/app/src-tauri/src/lifecycle.rs`.

### D5 — Relocate Settings behind a gear icon instead of always-inline (2026-07-23)
Why: user feedback that the scrollback + install/uninstall section sitting permanently visible at the bottom of the main sidebar "looks ugly and wrong." Alt: leave it inline — rejected per explicit user preference. Impact: `App.svelte` no longer mounts `<Settings>` inline in `.side`; a gear icon at the foot of the left panel opens the same `Settings.svelte` content in a popover/modal instead. Purely presentational — no change to `Settings.svelte`'s internal behavior. Files: `hub/app/src/App.svelte`, `hub/app/src/lib/Settings.svelte` (or a new wrapper component).
