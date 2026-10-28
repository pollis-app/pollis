#!/usr/bin/env bash
# Store-listing screenshots (docs/store-listing.md §7) with a real second person.
#
#   mobile/scripts/maestro-store-shots.sh <device-id> [out-dir]
#
# The peer is `pollis-tui`'s `store_shots_peer` example (a test-harness build
# that never ships): it signs up, waits for the phone to invite it to a group
# and open a DM with it, and posts its half of the conversation. The phone side
# is .maestro/two-client/store-shots/: sign-in shot + setup, converse, then one
# clean screenshot per listing screen (store-01 … store-06).
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). Display names come from the email local parts,
# so the accounts are adalovelace@… and bochen@…, never anything real.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-store-shots.sh <device-id> [out-dir]}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/store-shots"
STAMP="$(date +%Y%m%d%H%M%S)"
OUT="${2:-$HERE/.maestro/artifacts/store-$STAMP}"
mkdir -p "$OUT"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
# The display name is derived from the whole local part (a `+tag` ends up in
# it: probe+123@ → probe_123_xxxx), so keep the local part clean and make each
# run's address unique through the domain instead.
PRIMARY_EMAIL="adalovelace@shots$STAMP.example.com"
PEER_EMAIL="bochen@shots$STAMP.example.com"

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
(cd "$REPO" && cargo build -q -p pollis-tui --example store_shots_peer)
PEER_BIN="${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/store_shots_peer"
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

run() {
  mkdir -p "$OUT/$1"
  (cd "$OUT/$1" && maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
    -e MAESTRO_PEER_HANDLE="$HANDLE" --test-output-dir "$OUT/$1" "$FLOWS/$1.yaml")
}

echo "==> peer signs up ($PEER_EMAIL)"
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 90
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

# A clean status bar in every shot: a fixed 9:41 clock, full battery and
# signal, no notification icons. A simulator UDID means iOS (the same test as
# maestro-app-id.sh), where `simctl status_bar` does it; anything else is an
# adb serial, where Android's demo mode does.
if [[ "$DEVICE" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]]; then
  xcrun simctl status_bar "$DEVICE" override --time 9:41 --batteryState charged --batteryLevel 100 \
    --dataNetwork wifi --wifiMode active --wifiBars 3 --cellularMode notSupported
  trap 'kill "${PEER_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"; xcrun simctl status_bar "$DEVICE" clear' EXIT
else
  adb -s "$DEVICE" shell settings put global sysui_demo_allowed 1
  demo() { adb -s "$DEVICE" shell am broadcast -a com.android.systemui.demo -e command "$@" >/dev/null; }
  demo enter
  demo clock -e hhmm 0941
  demo battery -e level 100 -e plugged false
  demo network -e wifi show -e level 4 -e fully true
  demo network -e mobile hide
  demo notifications -e visible false
  trap 'kill "${PEER_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"; demo exit' EXIT
fi

echo "==> 1/3 phone signs up, makes the group, invites the peer, opens the DM"
run 1-setup
await_peer PEER_SENT 600

echo "==> 2/3 phone answers in the channel and the DM"
run 2-converse
await_peer PEER_DONE 300

echo "==> 3/3 screenshots"
run 3-shoot
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""

find "$OUT" -name 'store-*.png' -exec cp {} "$OUT/" \;
ls "$OUT"/store-*.png
