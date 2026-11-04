#!/usr/bin/env bash
# Who reacted (#1684): two accounts react to one message, ON ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-reaction-breakdown.sh <device-id>
#
# The peer is `pollis-tui`'s `reactions_peer` example (a test-harness build that
# never ships): it signs up a fresh account, waits for the device to open a DM
# with it, sends a message and reacts 👍 and 🎉. Part 2 has the device add its
# own 👍 with a short tap (the toggle must still work), long-press the pill and
# assert the sheet lists both accounts under the 👍 tab and only the peer under
# 🎉; the peer then has to see the device's 👍 before it exits 0.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). SHOTS_DIR=<dir> copies the flow's screenshots there.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-reaction-breakdown.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/reaction-breakdown"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-react-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-reactpeer-$STAMP@${BASE#*@}"

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
(cd "$REPO" && cargo build -q -p pollis-tui --example reactions_peer)
PEER_BIN="$REPO/target/debug/examples/reactions_peer"
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
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

DEBUG_ARGS=()
if [ -n "${SHOTS_DIR:-}" ]; then
  mkdir -p "$SHOTS_DIR"
  DEBUG_ARGS=(--test-output-dir "$SHOTS_DIR")
fi

echo "==> 1/2 device opens a DM with the peer"
maestro --device "$DEVICE" test "${DEBUG_ARGS[@]}" "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-open-dm.yaml"

echo "==> peer sends and reacts"
await_peer PEER_REACTED 300

echo "==> 2/2 device reacts too and opens the breakdown"
maestro --device "$DEVICE" test "${DEBUG_ARGS[@]}" "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/2-who-reacted.yaml"

echo "==> peer sees the device's reaction"
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
grep '^PEER_SAW' "$PEER_OUT"
echo "==> reaction-breakdown passed"
