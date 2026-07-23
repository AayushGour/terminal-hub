# Terminal Hub — npm distribution

Ship Terminal Hub so users can install it with **one cross-platform command**:

```bash
npm i -g @term-hub/term-hub      # macOS / Linux / Windows(*)
term-hub               # launches the GUI (also appears in the apps menu)
hub status                 # the CLI
```

`(*)` Windows binaries are phase 2 — the packaging is wired, the builds aren't enabled yet.

## How it's laid out (esbuild-style)

```
term-hub/              # the launcher package (published as `@term-hub/term-hub`)
  bin/term-hub.js      # resolves the host binary + launches the GUI (detached)
  bin/hub.js               # passthrough to the `hub` CLI
  lib/resolve.js           # picks @term-hub/<platform>-<arch>
  scripts/postinstall.js   # registers a launcher in the OS apps menu
  scripts/preuninstall.js  # removes it on `npm rm -g`
  assets/                  # icons (icns / png / ico)

packages/<platform>-<arch>/   # per-platform binaries (published as @term-hub/<key>)
  package.json                # os + cpu fields -> npm installs ONLY the host's one
  bin/                        # hub, hub-daemon, hub-relay, hub-app  (filled by CI)
```

Because each `@term-hub/*` package declares its `os`/`cpu`, npm downloads **only** the binary set matching the user's machine (they're `optionalDependencies` of the launcher).

The postinstall registers a clickable entry so it shows up like a normal app:

| OS | Entry |
|---|---|
| macOS | `~/Applications/Terminal Hub.app` → Launchpad / Spotlight |
| Linux | `~/.local/share/applications/term-hub.desktop` → app grid |
| Windows | Start Menu shortcut |

A macOS launcher `.app` created locally by postinstall isn't quarantined, so it opens with **no Gatekeeper warning** — no code-signing needed for the npm route.

## Releasing (maintainers)

Publishing is **automatic on merge to `main`**, gated on the version so merges
that don't change it are a no-op (npm rejects duplicate versions).

**One-time setup:**
1. Create the **`@term-hub` org** on npm (free, public scope).
2. Add an npm **automation** token as the `NPM_TOKEN` GitHub Actions secret.
3. Set `author` / `homepage` / repo URLs in `term-hub/package.json`.

**Each release:**
1. Bump `version` in `term-hub/package.json` (the source of truth — CI stamps
   it into every `@term-hub/*` package and the `optionalDependencies`).
2. Open a PR → merge to `main`.
3. `.github/workflows/release.yml` sees the new version, builds the binaries per
   platform (macOS arm64/x64, Linux x64/arm64), and `npm publish`es each
   `@term-hub/*` platform package plus the `@term-hub/term-hub` launcher.
   Publishing is idempotent — anything already on npm is skipped.

## Local smoke test (no publish)

```bash
# build the four binaries for your platform, then drop them in the matching pkg:
cd hub && cargo build --release -p hub-cli -p hub-daemon -p hub-relay
(cd app && npm ci && npm run build && npx tauri build --no-bundle)
KEY=$(node -e 'console.log(process.platform+"-"+process.arch)')
cp target/release/{hub,hub-daemon,hub-relay,hub-app} "npm/packages/$KEY/bin/"

# then install the launcher from the local folder:
npm i -g ./npm/term-hub
```
