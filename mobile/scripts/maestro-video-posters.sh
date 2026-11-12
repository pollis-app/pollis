#!/usr/bin/env bash
# Video posters across clients (#1837) on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-video-posters.sh <device-id>
#
# The peer is `pollis-tui`'s `video_poster_peer` example (a test-harness build
# that never ships, on the same pollis-core as desktop): it signs up a fresh
# account, waits for the device to open a DM with it, then sends two real
# videos through the outbox — one with a poster frame, one without, as a build
# from before #1837 sent them. Part 2 asserts the device draws a frame for
# both (`video-poster-pv`, `video-poster-local`) and sends a video itself; the
# peer then has to find a JPEG `pv` in that envelope before it exits 0.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, POLLIS_DEV_ACCESS_KEY for the dev gate and the dev R2 origins (both read
# from Doppler `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-video-posters.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client"
FIXTURES="$HERE/.maestro/fixtures"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-posters-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-posterspeer-$STAMP@${BASE#*@}"

dev_secret() {
  if command -v doppler >/dev/null; then
    doppler secrets get "$1" -p pollis -c dev_personal --plain 2>/dev/null || true
  fi
}
# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ]; then
  POLLIS_DEV_ACCESS_KEY="$(dev_secret POLLIS_DEV_ACCESS_KEY | cut -d, -f1)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer uploads real videos, so it needs the dev R2 origins.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-$(dev_secret R2_S3_ENDPOINT)}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-$(dev_secret R2_PUBLIC_URL)}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"
[ -n "$R2_S3_ENDPOINT" ] && [ -n "$R2_PUBLIC_URL" ] || { echo "R2_S3_ENDPOINT / R2_PUBLIC_URL unset" >&2; exit 1; }

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example video_poster_peer)
PEER_BIN="${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/video_poster_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
trap 'kill "${PEER_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

# Wait (up to $2 seconds) for a line the peer prints; fail if it dies first.
await_peer() {
  for _ in $(seq 1 "$2"); do
    grep -q "^$1" "$PEER_OUT" 2>/dev/null && return 0
    kill -0 "$PEER_PID" 2>/dev/null || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
    sleep 1
  done
  echo "peer never printed $1" >&2
  cat "$PEER_DIR/peer.err" >&2
  exit 1
}

echo "==> peer signs up ($PEER_EMAIL)"
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" \
  "$FIXTURES/composer-video.mp4" "$FIXTURES/composer-video-frame.jpg" \
  >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

echo "==> 1/2 device opens a DM with the peer"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/pins/1-open-dm.yaml"

echo "==> peer sends a video with a poster and one without"
await_peer PEER_SENT 300

echo "==> 2/2 device draws both frames and sends a video of its own"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/video-posters/2-posters.yaml"

echo "==> peer finds the device's poster"
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
grep '^PEER_SAW_POSTER' "$PEER_OUT"
echo "==> video posters passed"
