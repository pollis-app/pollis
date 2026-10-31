#!/usr/bin/env bash
# Typing indicator (#1635), on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-typing.sh <device-id>
#
# The peer is `pollis-tui`'s `typing_peer` example (feature `typing-peer`, which turns on
# pollis-core's real LiveKit path — a test build that never ships). It signs up a fresh
# account, accepts the DM the device opens with it, joins that DM's room, and then types or
# reports what the phone sent when a flow asks it over loopback HTTP
# (`.maestro/scripts/peer-typing.js`):
#   1. the peer types: "<peer> is typing…" shows over the composer; it stops: the line goes;
#   2. the device types: the peer sees `typing`; clearing the draft or sending: `stopped`.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled in, and
# POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-typing.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/typing"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-typing-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-typingpeer-$STAMP@${BASE#*@}"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer sends no media; these must be set but are never contacted. The SFU URL
# comes with each DS-minted token, not from LIVEKIT_URL.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building the peer (pollis-core with media)"
TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
(cd "$REPO" && cargo build -q -p pollis-tui --features typing-peer --example typing_peer)
PEER_BIN="$TARGET/debug/examples/typing_peer"
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
await_peer PEER_PORT= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
PORT="$(sed -n 's/^PEER_PORT=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

echo "==> 1/2 device opens a DM with the peer"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-open-dm.yaml"
await_peer PEER_READY 300

echo "==> 2/2 typing, both ways"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e PEER_PORT="$PORT" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/2-typing.yaml"
curl -fsS "http://127.0.0.1:$PORT/quit" >/dev/null || true
echo "==> typing passed"
