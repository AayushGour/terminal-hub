#!/usr/bin/env node
// Remove the desktop launcher created by postinstall.js on `npm rm -g`.
// Best-effort; never throws.
"use strict";

const fs = require("fs");
const os = require("os");
const path = require("path");

const NAME = "Terminal Hub";
const rm = (p) => {
  try {
    fs.rmSync(p, { recursive: true, force: true });
  } catch {
    /* ignore */
  }
};

if (process.platform === "darwin") {
  rm(path.join(os.homedir(), "Applications", `${NAME}.app`));
} else if (process.platform === "linux") {
  rm(path.join(os.homedir(), ".local", "share", "applications", "term-hub.desktop"));
  rm(path.join(os.homedir(), ".local", "share", "icons", "term-hub.png"));
} else if (process.platform === "win32") {
  const appData = process.env.APPDATA || path.join(os.homedir(), "AppData", "Roaming");
  rm(path.join(appData, "Microsoft", "Windows", "Start Menu", "Programs", `${NAME}.lnk`));
}
