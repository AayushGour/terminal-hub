#!/usr/bin/env node
// Passthrough to the `hub` CLI (install / status / kill / uninstall). Forwards
// args + stdio and mirrors the exit code.
"use strict";

const { spawn } = require("child_process");
const { exe } = require("../lib/resolve");

let bin;
try {
  bin = exe("hub");
} catch (e) {
  console.error(String(e.message || e));
  process.exit(1);
}

const child = spawn(bin, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (err) => {
  console.error(`hub: failed to run (${err.message}).`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 0);
});
