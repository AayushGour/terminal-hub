#!/usr/bin/env node
// Register a desktop launcher so Terminal Hub shows up in the OS apps menu
// (Launchpad / Linux app grid / Start Menu) and is clickable, not just runnable
// from a terminal. Best-effort: any failure here never fails `npm install`.
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");

const NAME = "Terminal Hub";
const pkgDir = path.resolve(__dirname, "..");
const assets = path.join(pkgDir, "assets");

function safe(label, fn) {
  try {
    fn();
  } catch (e) {
    console.error(`Terminal Hub: could not register ${label} (${e.message}). You can still run \`terminal-hub\`.`);
  }
}

// Resolve the GUI binary for this platform; if there isn't one, there's nothing
// to register (e.g. optional deps were skipped, or unsupported platform).
let appBin = null;
try {
  const p = require("../lib/resolve").exe("hub-app");
  if (fs.existsSync(p)) appBin = p;
} catch {
  /* no binary for this platform */
}
if (!appBin) process.exit(0);

if (process.platform === "darwin") registerMac();
else if (process.platform === "linux") registerLinux();
else if (process.platform === "win32") registerWindows();

function registerMac() {
  safe("the Applications entry", () => {
    const appDir = path.join(os.homedir(), "Applications", `${NAME}.app`);
    const macos = path.join(appDir, "Contents", "MacOS");
    const res = path.join(appDir, "Contents", "Resources");
    fs.mkdirSync(macos, { recursive: true });
    fs.mkdirSync(res, { recursive: true });

    const launcher = path.join(macos, "terminal-hub");
    fs.writeFileSync(launcher, `#!/bin/sh\nexec "${appBin}" "$@"\n`);
    fs.chmodSync(launcher, 0o755);

    const icns = path.join(assets, "icon.icns");
    if (fs.existsSync(icns)) fs.copyFileSync(icns, path.join(res, "icon.icns"));

    const plist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>${NAME}</string>
  <key>CFBundleDisplayName</key><string>${NAME}</string>
  <key>CFBundleIdentifier</key><string>dev.hub.launcher</string>
  <key>CFBundleExecutable</key><string>terminal-hub</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
`;
    fs.writeFileSync(path.join(appDir, "Contents", "Info.plist"), plist);
    // A launcher .app created locally (not downloaded) isn't quarantined, so it
    // opens with no Gatekeeper warning.
    console.log(`Terminal Hub: added to ~/Applications — find it in Launchpad/Spotlight.`);
  });
}

function registerLinux() {
  safe("the applications menu entry", () => {
    const appsDir = path.join(os.homedir(), ".local", "share", "applications");
    const iconsDir = path.join(os.homedir(), ".local", "share", "icons");
    fs.mkdirSync(appsDir, { recursive: true });
    fs.mkdirSync(iconsDir, { recursive: true });

    const png = path.join(assets, "icon.png");
    let icon = "utilities-terminal";
    if (fs.existsSync(png)) {
      fs.copyFileSync(png, path.join(iconsDir, "terminal-hub.png"));
      icon = "terminal-hub";
    }

    const desktop = `[Desktop Entry]
Type=Application
Name=${NAME}
Comment=Capture and manage all your terminals
Exec="${appBin}" %U
Icon=${icon}
Terminal=false
Categories=Utility;System;TerminalEmulator;
`;
    fs.writeFileSync(path.join(appsDir, "terminal-hub.desktop"), desktop);
    console.log("Terminal Hub: added to your applications menu.");
  });
}

function registerWindows() {
  safe("the Start Menu shortcut", () => {
    const { execFileSync } = require("child_process");
    const appData = process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
    const startMenu = path.join(appData, "Microsoft", "Windows", "Start Menu", "Programs");
    fs.mkdirSync(startMenu, { recursive: true });
    const lnk = path.join(startMenu, `${NAME}.lnk`);
    const ico = path.join(assets, "icon.ico");
    const q = (s) => s.replace(/'/g, "''");
    const ps =
      `$s=(New-Object -ComObject WScript.Shell).CreateShortcut('${q(lnk)}');` +
      `$s.TargetPath='${q(appBin)}';` +
      (fs.existsSync(ico) ? `$s.IconLocation='${q(ico)}';` : "") +
      `$s.Save()`;
    execFileSync("powershell", ["-NoProfile", "-NonInteractive", "-Command", ps], { stdio: "ignore" });
    console.log("Terminal Hub: added to the Start Menu.");
  });
}
