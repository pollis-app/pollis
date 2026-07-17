#!/usr/bin/env bash
#
# Virtual camera for the media E2E (issue #570, M3b).
#
# The camera-parity work (#568) captures a real webcam over V4L2 on Linux
# (pollis-capture-linux/src/camera.rs): it enumerates every /dev/videoN node
# that reports VIDEO_CAPTURE + at least one pixel format, opens the picked
# node, and prefers MJPG then falls back to raw YUYV at 1280x720. On a headless
# CI runner there is no webcam, so we synthesise one: load the `v4l2loopback`
# kernel module to create a fixed capture node (/dev/video0) and feed it a
# MOVING test pattern with ffmpeg in raw YUYV422 at 1280x720@30 — a real,
# changing signal in a format the app's YUYV path accepts directly (feeding
# YUYV means the loopback capture side offers ONLY YUYV, so the app
# deterministically takes its YUYV branch, no MJPEG decode in the loop).
#
# KNOWN RISK — v4l2loopback needs a kernel module (`modprobe`), which a
# GitHub-hosted runner MAY refuse (locked-down kernel / no matching headers for
# the DKMS build). This script therefore VERIFIES the loopback node actually
# exists after modprobe and FAILS LOUDLY with the modprobe/dmesg error if not,
# so a CI failure reads clearly as "v4l2loopback unavailable on this runner"
# rather than hanging or looking like a test bug. If it can't work headless on
# hosted runners, that's a legitimate finding — the outer agent may need to
# pivot to a self-hosted runner; this script makes that unambiguous.
#
# It emits POLLIS_E2E_CAMERA_DEVICE to $GITHUB_ENV in CI and as an `export ...`
# line on stdout locally (`eval "$(e2e/scripts/start-camera.sh)"`). All logging
# goes to stderr so stdout stays pure, evaluable `export` lines.
#
# Teardown is folded into e2e/scripts/stop-backend.sh (kills ffmpeg + removes
# the module).
set -euo pipefail

# Fixed device so the test + teardown agree without discovery. video_nr=0 →
# /dev/video0. On a headless runner this is the ONLY /dev/video* node, so the
# app's enumerate returns exactly one camera and toggleCamera() starts it
# directly without opening the picker (cameraActions.ts).
DEVICE="${POLLIS_E2E_CAMERA_DEVICE:-/dev/video0}"
VIDEO_NR="${POLLIS_E2E_CAMERA_NR:-0}"
CARD_LABEL="${POLLIS_E2E_CAMERA_LABEL:-Pollis Virtual Camera}"
WIDTH="${POLLIS_E2E_CAMERA_WIDTH:-1280}"
HEIGHT="${POLLIS_E2E_CAMERA_HEIGHT:-720}"
FPS="${POLLIS_E2E_CAMERA_FPS:-30}"
RUN_DIR="${POLLIS_E2E_CAMERA_DIR:-/tmp/pollis-e2e-camera}"

log() { echo "[start-camera] $*" >&2; }

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

command -v ffmpeg >/dev/null 2>&1 || {
  log "ERROR: ffmpeg not installed — add it via e2e/scripts/install-system-deps.sh"
  exit 1
}
command -v v4l2-ctl >/dev/null 2>&1 || {
  log "ERROR: v4l2-ctl not installed — add v4l-utils via e2e/scripts/install-system-deps.sh"
  exit 1
}

mkdir -p "$RUN_DIR"

# Kill any ffmpeg feeder from a prior run and unload the module so the load
# below is deterministic (a leftover producer would hold the device open).
if [ -f "$RUN_DIR/ffmpeg.pid" ]; then
  OLD_PID="$(cat "$RUN_DIR/ffmpeg.pid" 2>/dev/null || true)"
  if [ -n "${OLD_PID:-}" ]; then
    kill "$OLD_PID" >/dev/null 2>&1 || true
  fi
  rm -f "$RUN_DIR/ffmpeg.pid" || true
fi
pkill -9 -f "ffmpeg.*${DEVICE}" >/dev/null 2>&1 || true
$SUDO modprobe -r v4l2loopback >/dev/null 2>&1 || true

# The DKMS module is built against the RUNNING kernel's headers. On the CI
# runner $(uname -r) is the real kernel, so ensure its headers + the dkms
# package are present (install-system-deps.sh already tries this; this is a
# belt-and-suspenders retry for the exact running kernel). Best-effort — if the
# module is already available (modinfo succeeds) we skip the apt work.
if ! modinfo v4l2loopback >/dev/null 2>&1; then
  log "v4l2loopback module not found — installing headers + dkms for $(uname -r)"
  $SUDO apt-get update >&2 || true
  $SUDO apt-get install -y --no-install-recommends \
    "linux-headers-$(uname -r)" v4l2loopback-dkms v4l2loopback-utils >&2 || true
fi

# v4l2loopback links against the kernel V4L2 core (videodev.ko). GitHub-hosted
# runners boot a minimal Azure kernel WITHOUT videodev loaded, so loading
# v4l2loopback fails with "Unknown symbol video_ioctl2 / v4l2_* (err -2)".
# linux-modules-extra-$(uname -r) ships videodev.ko for the EXACT running kernel;
# install it and load videodev first so v4l2loopback's symbols resolve.
log "ensuring V4L2 core (videodev) is present for $(uname -r)"
$SUDO apt-get install -y --no-install-recommends "linux-modules-extra-$(uname -r)" >&2 || true
$SUDO modprobe videodev 2>>"$RUN_DIR/modprobe.err" || true

# Load the module: ONE device, fixed node, friendly label. exclusive_caps=1
# makes the node advertise CAPTURE (rather than both CAPTURE+OUTPUT at once)
# once a producer is streaming — the config real apps (Chrome/OBS→Zoom) rely
# on, and what lets the app's enumerate treat it as a plain capture device.
log "loading v4l2loopback (device $DEVICE, ${WIDTH}x${HEIGHT})"
if ! $SUDO modprobe v4l2loopback \
      devices=1 \
      video_nr="$VIDEO_NR" \
      card_label="$CARD_LABEL" \
      exclusive_caps=1 2> "$RUN_DIR/modprobe.err"; then
  log "ERROR: modprobe v4l2loopback FAILED — the loopback kernel module could not load."
  log "This usually means the GitHub-hosted runner won't build/load v4l2loopback"
  log "(no matching linux-headers-\$(uname -r) for the DKMS build, or module loading"
  log "is restricted). See the modprobe error below; a self-hosted runner may be needed."
  sed 's/^/[start-camera]   modprobe: /' "$RUN_DIR/modprobe.err" >&2 || true
  $SUDO dmesg 2>/dev/null | tail -n 20 | sed 's/^/[start-camera]   dmesg: /' >&2 || true
  exit 1
fi

# HARD verify the node materialised. With the module loaded the node exists
# immediately (before any producer); its absence means the load silently no-op'd.
if [ ! -e "$DEVICE" ]; then
  log "ERROR: v4l2loopback loaded but $DEVICE does not exist — loopback unavailable on this runner."
  $SUDO dmesg 2>/dev/null | tail -n 20 | sed 's/^/[start-camera]   dmesg: /' >&2 || true
  exit 1
fi
log "$DEVICE created"

# The loopback node is created root:video mode 0660, but the runner user (and
# the app that reads the device) isn't in the `video` group — ffmpeg then fails
# to OPEN it for writing with "Permission denied". Group changes need a new
# session mid-job, so just widen the node's perms: both the ffmpeg feeder
# (writer) and pollis' V4L2 capture (reader) run as the unprivileged user.
$SUDO chmod 0666 "$DEVICE" || true

# Feed a MOVING test pattern (testsrc: a scrolling gradient + a running
# frame/second counter) as raw YUYV422 at the app's preferred 1280x720@30. `-re`
# paces it in real time so it streams continuously like a real camera rather
# than blasting. Fully detached (setsid + nohup + </dev/null) so it outlives
# THIS workflow step — a plain `&` would be reaped with the step's process group
# and the device would go idle before the app opens it.
log "starting ffmpeg feeder (testsrc → $DEVICE, YUYV422 ${WIDTH}x${HEIGHT}@${FPS})"
setsid nohup ffmpeg -hide_banner -loglevel warning -nostdin -re \
  -f lavfi -i "testsrc=size=${WIDTH}x${HEIGHT}:rate=${FPS}" \
  -pix_fmt yuyv422 -f v4l2 "$DEVICE" \
  > "$RUN_DIR/ffmpeg.log" 2>&1 < /dev/null &
FFMPEG_PID=$!
echo "$FFMPEG_PID" > "$RUN_DIR/ffmpeg.pid"
disown "$FFMPEG_PID" 2>/dev/null || true

# Wait until the CAPTURE side actually offers a format — i.e. the producer is
# streaming and the app's enumerate would find a usable device. Never sleep for
# correctness; poll. Fail loudly (with the ffmpeg log) if it never comes up.
log "waiting for $DEVICE to advertise a capture format..."
ready=0
for _ in $(seq 1 30); do
  # ffmpeg must still be alive — if it died (e.g. bad pixfmt), stop waiting.
  if ! kill -0 "$FFMPEG_PID" >/dev/null 2>&1; then
    log "ERROR: ffmpeg feeder exited early — see log:"
    cat "$RUN_DIR/ffmpeg.log" >&2 || true
    exit 1
  fi
  if v4l2-ctl --device="$DEVICE" --list-formats 2>/dev/null | grep -qiE "YUYV|YU12|MJPG"; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  log "ERROR: $DEVICE never advertised a capture format after ffmpeg started."
  log "ffmpeg log:"
  cat "$RUN_DIR/ffmpeg.log" >&2 || true
  exit 1
fi

# Proof dump: the device list + the negotiated capture format, mirroring the
# HARD-verify dumps in start-audio.sh.
log "v4l2-ctl --list-devices:"
v4l2-ctl --list-devices >&2 || true
log "v4l2-ctl --device=$DEVICE --list-formats:"
v4l2-ctl --device="$DEVICE" --list-formats >&2 || true
log "virtual camera up on $DEVICE"

# --- emit the env (documentational; the app discovers the device itself) ----
emit() {
  local key="$1" value="$2"
  echo "export ${key}=${value}"
  if [ -n "${GITHUB_ENV:-}" ]; then
    echo "${key}=${value}" >> "$GITHUB_ENV"
  fi
}
emit POLLIS_E2E_CAMERA_DEVICE "$DEVICE"
log "virtual camera ready"
