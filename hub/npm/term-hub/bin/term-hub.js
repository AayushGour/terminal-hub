#!/usr/bin/env node
// Launch the Terminal Hub GUI. Detaches so the terminal returns immediately.
"use strict";

const { spawn } = require("child_process");
const { exe } = require("../lib/resolve");

let bin;
try {
  bin = exe("hub-app");
} catch (e) {
  console.error(String(e.message || e));
  process.exit(1);
}

// On Linux the GUI needs the webkit2gtk runtime; hint if it's obviously missing.
if (process.platform === "linux") {
  const fs = require("fs");
  const hasWebkit = ["/usr/lib", "/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64"].some(
    (d) => {
      try {
        return fs.readdirSync(d).some((f) => f.startsWith("libwebkit2gtk-4.1"));
      } catch {
        return false;
      }
    }
  );
  if (!hasWebkit) {
    console.error(
      "Terminal Hub: the webkit2gtk runtime seems missing. If the window doesn't open, install it:\n" +
        "  Debian/Ubuntu:  sudo apt install libwebkit2gtk-4.1-0\n" +
        "  Fedora:         sudo dnf install webkit2gtk4.1"
    );
  }
}

const child = spawn(bin, process.argv.slice(2), {
  stdio: "ignore",
  detached: true,
});
child.on("error", (err) => {
  console.error(`Terminal Hub: failed to launch (${err.message}).`);
  process.exit(1);
});
child.unref(); // let the terminal return; the GUI keeps running
