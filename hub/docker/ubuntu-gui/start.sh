#!/usr/bin/env bash
# Boot an Ubuntu 24.04 XFCE desktop showing the hub GUI, viewable in a browser
# via noVNC. Terminals opened on the desktop are captured by the rc hook.
set -u
export HOME=/root
export DISPLAY=:0
export SHELL=/usr/bin/zsh
export HUB_SKIP_SERVICE_ACTIVATION=1        # no systemd in a container
# webkit needs a software-GL driver (mesa/llvmpipe) under Xvfb or it paints blank
export LIBGL_ALWAYS_SOFTWARE=1
export GALLIUM_DRIVER=llvmpipe
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1
export WEBKIT_DISABLE_SANDBOX=1
BINS=/build/hub/target/release

log() { echo "[start] $*"; }

eval "$(dbus-launch --sh-syntax)"          # session bus (xfce + webkit want one)
touch /root/.zshrc /root/.bashrc           # rc files for the capture hook

log "installing hub (rc hook + ~/.hub/bin)..."
"$BINS/hub" install --yes --bin-src "$BINS" || log "hub install rc=$?"

log "starting daemon..."
"$HOME/.hub/bin/hub-daemon" >/tmp/hubd.log 2>&1 &

log "starting Xvfb..."
Xvfb :0 -screen 0 1600x900x24 -ac >/tmp/xvfb.log 2>&1 &
for i in $(seq 1 40); do xdpyinfo -display :0 >/dev/null 2>&1 && break; sleep 0.25; done

log "starting XFCE desktop..."
startxfce4 >/tmp/xfce.log 2>&1 &
sleep 5

log "starting VNC + noVNC on :8080..."
x11vnc -display :0 -nopw -forever -shared -rfbport 5900 >/tmp/x11vnc.log 2>&1 &
websockify --web=/usr/share/novnc 8080 localhost:5900 >/tmp/novnc.log 2>&1 &

sleep 1
log "launching hub-app..."
"$BINS/hub-app" >/tmp/hubapp.log 2>&1 &

# open a captured terminal (zsh sources ~/.zshrc -> hook -> capture)
sleep 5
log "opening a captured terminal..."
xfce4-terminal --geometry=110x28 --command="zsh" >/tmp/term.log 2>&1 &

echo "==================================================================="
echo "  Ubuntu 24.04 + XFCE running the hub GUI. Open in your browser:"
echo "    http://localhost:8080/vnc.html   (click Connect)"
echo "==================================================================="

tail -F /tmp/hubapp.log /tmp/hubd.log 2>/dev/null
