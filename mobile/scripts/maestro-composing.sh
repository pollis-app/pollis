#!/usr/bin/env bash
# A message arriving while the user types (#1507) on ONE device plus a dev-DS
# peer.
#
#   mobile/scripts/maestro-composing.sh <device-id>
#
# The peer is `pollis-tui`'s `composing_peer` example (a test-harness build
# that never ships): it signs up a fresh account, accepts the device's invite
# to a group and posts more than a screen of history in #General. The device then types a draft with the
# keyboard up and stops there; only then does the peer send three lines into
# the open channel. With the keyboard still up, the composer must still be
# above it: the draft is sent by tapping Send where it is (a Send pushed behind
# the keyboard gets a key press instead, and the draft never lands). Twice, so
# the second arrival comes after a send and a scroll.
#
# Each arrival is also shot straight off the device (adb / simctl) before
# Maestro touches anything: device-<n>-arrived.png in the output directory.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail

DEVICE="${1:?usage: maestro-composing.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/composing"
OUT="${OUT:-$HERE/.maestro/artifacts/$(date +%Y-%m-%d)/composing-$DEVICE}"
mkdir -p "$OUT"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-composing-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-composingpeer-$STAMP@${BASE#*@}"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer only sends text; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example composing_peer)
PEER_BIN="$REPO/target/debug/examples/composing_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export COMPOSING_PEER_SIGNAL_DIR="$PEER_DIR/signals"
mkdir -p "$COMPOSING_PEER_SIGNAL_DIR"
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

# A screenshot straight off the device, no Maestro step in between.
shoot() {
  if [[ "$DEVICE" == emulator-* ]]; then
    adb -s "$DEVICE" exec-out screencap -p > "$OUT/$1.png"
  else
    xcrun simctl io "$DEVICE" screenshot "$OUT/$1.png" >/dev/null
  fi
}

run_flow() {
  maestro --device "$DEVICE" test --debug-output "$OUT/.debug" "${ENV_ARGS[@]}" "$@"
}

echo "==> peer signs up"
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

echo "==> 1 device makes a group and invites the peer"
run_flow -e MAESTRO_EMAIL="$PRIMARY_EMAIL" -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-setup.yaml"
await_peer PEER_READY 300

for n in 1 2; do
  echo "==> $n device types a draft and leaves the keyboard up"
  run_flow -e DRAFT="my draft $n" "$FLOWS/2-type.yaml"
  shoot "device-$n-typing"
  echo "==> peer sends while the device is typing"
  touch "$COMPOSING_PEER_SIGNAL_DIR/go-$n"
  await_peer "PEER_SENT_$n" 120
  # Let the last line arrive and the list settle before the shot.
  sleep 5
  shoot "device-$n-arrived"
  echo "==> the composer is still above the keyboard: Send it from there"
  run_flow -e BATCH="$n" -e DRAFT="my draft $n" "$FLOWS/3-send.yaml"
done
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""

find "$OUT/.debug" \( -path "*/takeScreenshot/*.png" -o -path "*/screenshots/*.png" \) -exec cp {} "$OUT/" \; 2>/dev/null || true
rm -rf "$OUT/.debug"
echo "==> composing passed — screenshots in $OUT"
