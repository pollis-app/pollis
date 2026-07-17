#!/usr/bin/env bash
#
# Virtual audio for the media E2E (issue #570, M3a).
#
# A real 1:1 call join publishes a microphone track: `join_voice_channel`
# (pollis-core/src/commands/voice/lifecycle.rs) opens a cpal INPUT stream and,
# for playback, a cpal OUTPUT stream — and it FAILS THE JOIN if either device
# can't open (`mic_res...??` at the tokio::join!). On a headless CI runner there
# is no sound card, so we stand up a software one: a PulseAudio daemon with a
# null sink (a fake speaker) and a virtual source (a fake mic), and point ALSA's
# default PCM at PulseAudio so cpal — which uses the ALSA host on Linux — opens
# the virtual devices instead of failing on "no such device".
#
# It emits the env the app needs (PULSE_SERVER / PULSE_SINK / PULSE_SOURCE) to
# $GITHUB_ENV in CI and as `export ...` lines on stdout locally
# (`eval "$(e2e/scripts/start-audio.sh)"`). All logging goes to stderr so stdout
# stays pure, evaluable `export` lines.
#
# Teardown is folded into e2e/scripts/stop-backend.sh (kills the daemon).
set -euo pipefail

# Fixed, world-known socket path so the app process — which runs inside a
# SEPARATE dbus session (`dbus-run-session` in the desktop-e2e action) and so
# can't discover PulseAudio over the session bus — reaches the same daemon via
# an explicit PULSE_SERVER. auth-anonymous on the socket means no cookie sharing
# across sessions is required either.
PULSE_DIR="${POLLIS_E2E_PULSE_DIR:-/tmp/pollis-e2e-pulse}"
PULSE_SOCKET="$PULSE_DIR/native"
PULSE_SERVER_ADDR="unix:$PULSE_SOCKET"
# Virtual device names (also the client-side PULSE_SINK/PULSE_SOURCE defaults).
SINK_NAME="pollis_vspeaker"
MIC_SINK_NAME="pollis_vmic_sink"
SOURCE_NAME="pollis_vmic"

log() { echo "[start-audio] $*" >&2; }

command -v pulseaudio >/dev/null 2>&1 || {
  log "ERROR: pulseaudio not installed — add it via e2e/scripts/install-system-deps.sh"
  exit 1
}
command -v pactl >/dev/null 2>&1 || {
  log "ERROR: pactl not installed — add pulseaudio-utils via e2e/scripts/install-system-deps.sh"
  exit 1
}

# Clear any daemon/socket from a prior run so module-load state is deterministic.
log "resetting any prior PulseAudio daemon"
PULSE_RUNTIME_PATH="$PULSE_DIR" pulseaudio --kill >/dev/null 2>&1 || true
pkill -9 -x pulseaudio >/dev/null 2>&1 || true
rm -rf "$PULSE_DIR"
mkdir -p "$PULSE_DIR"

# Start a private daemon: `-n` skips the system default.pa (which would probe
# real hardware and can hang / spew errors headless); we load exactly the
# modules we need. `--exit-idle-time=-1` keeps it alive with no clients so it
# doesn't race the app's first connect. The null sinks + remapped source give a
# working speaker and mic that need no hardware.
log "starting PulseAudio daemon (socket $PULSE_SOCKET)"
PULSE_RUNTIME_PATH="$PULSE_DIR" pulseaudio \
  --daemonize=yes \
  --exit-idle-time=-1 \
  --disallow-exit=yes \
  --disable-shm=yes \
  -n \
  --load="module-native-protocol-unix auth-anonymous=1 socket=$PULSE_SOCKET" \
  --load="module-null-sink sink_name=$SINK_NAME sink_properties=device.description=Pollis_Virtual_Speaker" \
  --load="module-null-sink sink_name=$MIC_SINK_NAME sink_properties=device.description=Pollis_Virtual_Mic_Sink" \
  --load="module-remap-source master=$MIC_SINK_NAME.monitor source_name=$SOURCE_NAME source_properties=device.description=Pollis_Virtual_Mic" \
  --load="module-always-sink"

export PULSE_SERVER="$PULSE_SERVER_ADDR"

# Wait until the daemon answers on its socket (never sleep-for-correctness).
log "waiting for PulseAudio to accept connections..."
ready=0
for _ in $(seq 1 30); do
  if PULSE_SERVER="$PULSE_SERVER_ADDR" pactl info >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  log "ERROR: PulseAudio did not come up on $PULSE_SOCKET"
  exit 1
fi

# Make the virtual devices the defaults so cpal's default_input/output_device
# (the app passes input_device: None) resolves to them.
pactl --server "$PULSE_SERVER_ADDR" set-default-sink "$SINK_NAME" >&2
pactl --server "$PULSE_SERVER_ADDR" set-default-source "$SOURCE_NAME" >&2

# HARD verify: a sink AND a source must exist, or the join will fail later with
# a much harder-to-diagnose cpal error deep in the app. Fail loudly here instead.
log "pactl list short sinks:"
pactl --server "$PULSE_SERVER_ADDR" list short sinks >&2 || true
log "pactl list short sources:"
pactl --server "$PULSE_SERVER_ADDR" list short sources >&2 || true

if ! pactl --server "$PULSE_SERVER_ADDR" list short sinks | grep -q "$SINK_NAME"; then
  log "ERROR: virtual sink '$SINK_NAME' missing after setup"
  exit 1
fi
if ! pactl --server "$PULSE_SERVER_ADDR" list short sources | grep -q "$SOURCE_NAME"; then
  log "ERROR: virtual source '$SOURCE_NAME' missing after setup"
  exit 1
fi
log "virtual sink '$SINK_NAME' + source '$SOURCE_NAME' up"

# Route ALSA's default PCM/CTL at PulseAudio so cpal (Linux = ALSA host) opens
# our virtual devices via the ALSA→Pulse plugin (libasound2-plugins). System-
# wide so it's unambiguous regardless of which HOME the app runs under. Needs
# sudo on the runner (passwordless); no-op-safe if already correct.
ASOUND_CONF="/etc/asound.conf"
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi
log "pointing ALSA default at PulseAudio ($ASOUND_CONF)"
$SUDO tee "$ASOUND_CONF" >/dev/null <<'EOF'
# Written by e2e/scripts/start-audio.sh (issue #570, M3a): route ALSA's default
# PCM/CTL through PulseAudio so cpal (which uses the ALSA host on Linux) opens
# the virtual sink/source instead of failing on missing hardware.
pcm.!default {
  type pulse
}
ctl.!default {
  type pulse
}
EOF

# --- emit the env the app consumes ------------------------------------------
# PULSE_SERVER: explicit socket (survives the app's separate dbus session).
# PULSE_SINK / PULSE_SOURCE: per-client default overrides (belt-and-suspenders
# alongside the pactl set-default above and the ALSA default route).
emit() {
  local key="$1" value="$2"
  echo "export ${key}=${value}"
  if [ -n "${GITHUB_ENV:-}" ]; then
    echo "${key}=${value}" >> "$GITHUB_ENV"
  fi
}
emit PULSE_SERVER "$PULSE_SERVER_ADDR"
emit PULSE_RUNTIME_PATH "$PULSE_DIR"
emit PULSE_SINK "$SINK_NAME"
emit PULSE_SOURCE "$SOURCE_NAME"
log "virtual audio ready"
