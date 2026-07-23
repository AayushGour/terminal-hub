# Ubuntu 24.04 + XFCE desktop running the hub GUI, streamed to your browser via
# noVNC. Great for trying hub without touching your own machine.
#
# Build context MUST be the workspace root (the `hub/` dir):
#   cd hub
#   docker build -f docker/ubuntu-gui/Dockerfile -t hub-ubuntu-gui .
#   docker run -d --name hub-gui -p 8080:8080 --shm-size=1g hub-ubuntu-gui
#   open http://localhost:8080/vnc.html   (click Connect)
#
# Inside the desktop, any terminal you open is captured and appears in the hub.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
# Build tooling + Tauri (webkit/gtk) + a real XFCE desktop + virtual display +
# VNC/noVNC + software-GL (mesa/llvmpipe) so webkit renders headless under Xvfb.
RUN apt-get update && apt-get install -y --no-install-recommends \
      curl ca-certificates build-essential pkg-config libssl-dev git file \
      libwebkit2gtk-4.1-dev libgtk-3-dev libayatana-appindicator3-dev librsvg2-dev libglib2.0-dev \
      xvfb x11vnc novnc websockify dbus-x11 x11-utils scrot \
      xfce4 xfce4-terminal xfce4-goodies \
      libgl1-mesa-dri libglx-mesa0 libegl-mesa0 mesa-utils \
      nodejs npm zsh procps \
 && rm -rf /var/lib/apt/lists/*

# Rust (Ubuntu ships no toolchain by default)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /build/hub
# Copy the workspace (target/ + node_modules excluded via .dockerignore)
COPY . /build/hub

# 1) engine binaries
RUN cargo build --release -p hub-cli -p hub-daemon -p hub-relay \
 && mkdir -p app/src-tauri/binaries \
 && cp target/release/hub target/release/hub-daemon target/release/hub-relay app/src-tauri/binaries/

# 2) frontend (vite -> app/dist)
RUN cd app && npm ci && npm run build

# 3) the Tauri app binary in PROD mode. Must go through the tauri CLI (not a raw
#    `cargo build`) so the frontend is EMBEDDED instead of the app trying to
#    reach the vite dev server. `--no-bundle` skips flaky .deb/AppImage tooling.
RUN cd app && npx tauri build --no-bundle

COPY docker/ubuntu-gui/start.sh /start.sh
RUN chmod +x /start.sh

EXPOSE 8080
CMD ["/start.sh"]
