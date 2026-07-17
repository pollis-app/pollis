#!/usr/bin/env bash
#
# Single source of truth for the desktop-E2E system dependencies.
#
# Both e2e/Dockerfile (build time) and .github/actions/desktop-e2e (run time on
# a bare ubuntu-24.04 runner) call THIS script, so the apt list is defined once
# and never drifts between the image and CI. Keep it in lockstep with the
# reasons documented inline in .github/workflows/e2e-smoke.yml.
#
# Targets ubuntu-24.04 (see e2e-smoke.yml / mls-tests.yml: 24.04, not 22.04,
# because src-tauri/build.rs compiles pollis-capture-linux, whose libspa 0.9
# bindgen needs PipeWire >= 1.0 headers that 24.04 ships and 22.04 doesn't).
#
# Runs as root inside the Docker build; falls back to sudo on the runner where
# the default user is unprivileged.
set -euo pipefail

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends \
  `# --- webview: Tauri's WebKitGTK window + the WebKitWebDriver tauri-driver drives ---` \
  libwebkit2gtk-4.1-dev \
  webkit2gtk-driver \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libxdo-dev \
  libssl-dev \
  `# --- audio: pollis-core links the media stack (cpal/pulse/pipewire) even when unused ---` \
  libasound2-dev \
  libpulse-dev \
  libpipewire-0.3-dev \
  libva-dev \
  `# --- virtual audio (M3a): a headless PulseAudio + null-sink/virtual-source so a real ---` \
  `# --- call's cpal mic/playback opens; libasound2-plugins gives ALSA the pulse plugin ---` \
  `# --- so cpal (ALSA host on Linux) routes to PulseAudio. See e2e/scripts/start-audio.sh ---` \
  pulseaudio \
  pulseaudio-utils \
  libasound2-plugins \
  `# --- screenshare-xcb: forward-compat for the capture path later milestones add (M1+) ---` \
  libxcb1-dev \
  libxcb-shm0-dev \
  libxcb-randr0-dev \
  `# --- dbus/keystore: libdbus headers + a daemon so the run-time dbus session can start ---` \
  libdbus-1-dev \
  dbus \
  dbus-x11 \
  `# --- build tools: meson deps for webrtc-audio-processing-sys's vendored C++ build ---` \
  cmake \
  clang \
  ninja-build \
  `# --- display: no real X server on the runner / in the image ---` \
  xvfb
