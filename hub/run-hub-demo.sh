#!/bin/bash
# Safe manual test of the packaged hub GUI — touches NOTHING in your real env.
# Runs a daemon under a throwaway HUB_DIR and launches the .app pointed at it.
# No rc-file edits, no launchd, no real ~/.hub. Ctrl-C to tear everything down.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"          # hub/
REL="$HERE/target/release"
APP="$REL/bundle/macos/hub.app/Contents/MacOS/hub-app"

for b in "$REL/hub-daemon" "$REL/hub-relay" "$APP"; do
  [ -x "$b" ] || { echo "missing: $b — run: cargo build --release -p hub-daemon -p hub-relay && npm --prefix app run tauri build"; exit 1; }
done

export HUB_DIR="$(mktemp -d /tmp/hub-demo.XXXX)"   # isolated sandbox
export PATH="$REL:$PATH"                           # so the app can spawn hub-relay
echo "sandbox HUB_DIR = $HUB_DIR"

cleanup() {
  echo; echo "tearing down…"
  kill "$DPID" 2>/dev/null
  pkill -f "$REL/hub-relay" 2>/dev/null
  rm -rf "$HUB_DIR"
  echo "done — real environment untouched."
}
trap cleanup EXIT INT TERM

echo "starting daemon…"
"$REL/hub-daemon" >"$HUB_DIR/daemon.log" 2>&1 &
DPID=$!
for i in $(seq 1 100); do [ -S "$HUB_DIR/hubd.sock" ] && break; sleep 0.05; done
echo "daemon up (pid $DPID). token: $(ls -l "$HUB_DIR/token" | awk '{print $1}')"

echo
echo "launching the app.  In the window, try:"
echo "  • '+ New session'  → a Hub session appears in the list"
echo "  • click it         → a live terminal tile opens; type: echo hi; ls; top"
echo "  • open a 2nd       → two shells stream independently"
echo "  • focus + drag-resize a tile → the shell's stty size follows"
echo "  • SPOF: in another terminal run  kill -9 $DPID ,"
echo "    then restart with  HUB_DIR=$HUB_DIR $REL/hub-daemon &  → reattach a tile"
echo
echo "close the app window (or Ctrl-C here) to tear it all down."
"$APP"    # inherits HUB_DIR + PATH; blocks until the window closes
