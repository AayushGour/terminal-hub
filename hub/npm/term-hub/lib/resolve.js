// Resolve the prebuilt binary for the host platform/arch.
//
// The binaries live in a per-platform optional dependency
// (`@terminal-hub/<platform>-<arch>`); npm only installs the one matching the
// host (via each package's `os`/`cpu` fields), so exactly one is present.
"use strict";

const path = require("path");

// Platforms with published prebuilt binaries. Windows (win32-x64) is phase 2 —
// its package/CI are wired but not built yet, so it's intentionally absent here
// (Windows users get a clear "in progress" message rather than a broken install).
const SUPPORTED = new Set([
  "darwin-arm64",
  "darwin-x64",
  "linux-x64",
  "linux-arm64",
]);

const KEY = `${process.platform}-${process.arch}`;
const PKG = `@terminal-hub/${KEY}`;
const EXT = process.platform === "win32" ? ".exe" : "";

/** Directory holding the prebuilt binaries for this platform, or null. */
function binDir() {
  try {
    // Resolve the platform package relative to THIS package (works whether
    // installed globally or locally).
    const manifest = require.resolve(`${PKG}/package.json`, { paths: [__dirname] });
    return path.join(path.dirname(manifest), "bin");
  } catch {
    return null;
  }
}

/** Absolute path to a named binary (e.g. "hub-app", "hub"), or throws a clear error. */
function exe(name) {
  if (!SUPPORTED.has(KEY)) {
    const windows = process.platform === "win32";
    throw new Error(
      windows
        ? `Terminal Hub: Windows support is in progress — no prebuilt binary yet.\n` +
            `Track it: https://github.com/your-org/terminal-hub#roadmap--vision`
        : `Terminal Hub: no prebuilt binary for ${KEY}.\n` +
            `Supported: ${[...SUPPORTED].join(", ")}.\n` +
            `Build from source instead: https://github.com/your-org/terminal-hub#option-b--build-from-source`
    );
  }
  const dir = binDir();
  if (!dir) {
    throw new Error(
      `Terminal Hub: the binary package ${PKG} isn't installed.\n` +
        `Reinstall with:  npm install -g terminal-hub  (optional deps must be allowed)`
    );
  }
  return path.join(dir, name + EXT);
}

module.exports = { KEY, PKG, SUPPORTED, binDir, exe };
