#!/usr/bin/env bash
# The DM list's order (#1838), on ONE device plus two dev-DS peers.
#
#   mobile/scripts/maestro-dm-order.sh <device-id>
#
# The peers are two `pollis-tui` `live_message_peer` examples (test-harness builds that never
# ship): each signs up a fresh account, accepts the DM the device opens with it, and then sends
# whenever a flow asks it over loopback HTTP (`.maestro/scripts/peer-send.js`):
#   1. the device opens a DM with peer A, then one with peer B, sending into each;
#   2. on the Direct list B (newest) is above A; A replies and moves to the top live; the device
#      sends into B and, back on the list, B is on top again.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled in, and
# POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-dm-order.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/dm-order"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-order-$STAMP@${BASE#*@}"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peers only send text; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example live_message_peer)
TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
PEER_BIN="$TARGET/debug/examples/live_message_peer"
PEER_DIR="$(mktemp -d)"
trap 'kill "${PID_A:-}" "${PID_B:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

# Wait (up to $3 seconds) for peer $1 to print a line starting $2; fail if it dies first.
await_peer() {
  local pid_var="PID_$1"
  for _ in $(seq 1 "$3"); do
    grep -q "^$2" "$PEER_DIR/$1.out" 2>/dev/null && return 0
    kill -0 "${!pid_var}" 2>/dev/null || { cat "$PEER_DIR/$1.err" >&2; exit 1; }
    sleep 1
  done
  echo "peer $1 never printed $2" >&2
  cat "$PEER_DIR/$1.err" >&2
  exit 1
}

for p in A B; do
  lower="$(echo "$p" | tr 'A-Z' 'a-z')"
  email="${BASE%@*}-orderpeer$lower-$STAMP@${BASE#*@}"
  echo "==> peer $p signs up ($email)"
  POLLIS_DATA_DIR="$PEER_DIR/data-$p" "$PEER_BIN" "$email" >"$PEER_DIR/$p.out" 2>"$PEER_DIR/$p.err" &
  eval "PID_$p=$!"
done
await_peer A PEER_PORT= 60
await_peer B PEER_PORT= 60
HANDLE_A="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_DIR/A.out")"
HANDLE_B="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_DIR/B.out")"
PORT_A="$(sed -n 's/^PEER_PORT=//p' "$PEER_DIR/A.out")"
PORT_B="$(sed -n 's/^PEER_PORT=//p' "$PEER_DIR/B.out")"
echo "    peer handles: $HANDLE_A, $HANDLE_B"

echo "==> 1/2 device opens a DM with each peer"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e PEER_A_HANDLE="$HANDLE_A" -e PEER_B_HANDLE="$HANDLE_B" "$FLOWS/1-open-dms.yaml"
await_peer A PEER_READY 300
await_peer B PEER_READY 300

echo "==> 2/2 newest first, and the live moves"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e PEER_A_HANDLE="$HANDLE_A" \
  -e PEER_B_HANDLE="$HANDLE_B" -e PEER_A_PORT="$PORT_A" "$FLOWS/2-order.yaml"
curl -fsS "http://127.0.0.1:$PORT_A/quit" >/dev/null || true
curl -fsS "http://127.0.0.1:$PORT_B/quit" >/dev/null || true
echo "==> dm order passed"
