# Reusable build environment for the native desktop E2E suite (issue #570, M0).
#
# This image packages the EXACT environment .github/workflows/e2e-smoke.yml
# provisions inline, so the smoke — and later milestones (backend fixtures,
# two-client, media) that extend it — can run identically in CI AND on a plain
# Docker host (a dev machine, or a CI-less sandbox).
#
# It is a BUILD-ENVIRONMENT image: the repo is NOT COPYed in. Mount the source
# at run time (`-v "$PWD":/work -w /work`) so one image is reusable across
# branches without a rebuild. See e2e/README.md "Run the environment locally".
#
# Pinned inputs (bump these in lockstep with the rest of the repo):
#   - base:  ubuntu-24.04  (24.04 not 22.04 — pollis-capture-linux's libspa 0.9
#            bindgen needs PipeWire >= 1.0 headers only 24.04 ships)
#   - Node:  20            (matches e2e-smoke.yml's setup-node)
#   - pnpm:  10.25.0       (root package.json `packageManager`)
#   - Rust:  1.96.0        (rust-toolchain.toml — reproducible-build pin)
#   - meson: >= 1.3        (newer than apt; webrtc-audio-processing-sys build)
#
# System deps come from e2e/scripts/install-system-deps.sh — the SINGLE place
# the apt list lives, shared with the composite action so it never drifts. Dep
# groups (webview / audio / screenshare-xcb / dbus-keystore / build-tools /
# display) are documented in that script.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

# WebKitGTK gotchas, baked in so every `docker run` inherits them (same as
# `pnpm dev` and e2e/smoke.js): without disabled compositing + the x11 GDK
# backend, WebKitGTK's compositor wedges the WebView on this headless setup —
# pages half-render and the screenshot endpoint hangs forever.
ENV WEBKIT_DISABLE_COMPOSITING_MODE=1 \
    GDK_BACKEND=x11 \
    XDG_SESSION_TYPE=x11

# System packages (webview / audio / screenshare-xcb / dbus / build-tools /
# xvfb) + the forward-compat xcb libs. Copy only the script so this layer is
# cached independently of the rest of the tree.
COPY e2e/scripts/install-system-deps.sh /tmp/install-system-deps.sh
RUN bash /tmp/install-system-deps.sh \
  && rm -rf /var/lib/apt/lists/* /tmp/install-system-deps.sh

# Node 20 + pnpm (via corepack, pinned to the repo's packageManager version).
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates curl gnupg git python3 python3-pip \
  && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
  && apt-get install -y --no-install-recommends nodejs \
  && corepack enable \
  && corepack prepare pnpm@10.25.0 --activate \
  && rm -rf /var/lib/apt/lists/*

# meson newer than apt (>= 1.3), same as e2e-smoke.yml's dedicated step.
# --break-system-packages: Ubuntu 24.04's system Python is PEP 668 externally
# managed; this is a throwaway build image, so a global install is fine.
RUN python3 -m pip install --break-system-packages 'meson>=1.3'

# Pinned Rust toolchain (rust-toolchain.toml). Installed explicitly (not lazily
# on first cargo) so `cargo install tauri-driver` below has a compiler, and so
# a mounted checkout builds against the exact pinned rustc.
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
      | sh -s -- -y --profile minimal --default-toolchain 1.96.0 \
  && rustc --version

# tauri-driver: the WebDriver proxy the harness talks to (drives WebKitWebDriver).
RUN cargo install tauri-driver --locked

WORKDIR /work
