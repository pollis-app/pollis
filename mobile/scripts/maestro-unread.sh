#!/usr/bin/env bash
# Unread for messages that arrive while the app is away (#1422) on ONE device
# plus a dev-DS peer.
#
#   mobile/scripts/maestro-unread.sh <device-id>
#
# The peer is `pollis-tui`'s `unread_peer` example (a test-harness build that
# never ships): it signs up a fresh account, waits for the device to open a DM
# with it, and then sends twice, each time only when this script says so:
#   1. while the app is in the BACKGROUND — on resume, the Direct tab badge and
#      the DM row's count must show the two messages;
#   2. while the app is CLOSED — after a cold start and the PIN, the count must
#      be three (the badge used to start from zero on every launch);
# then opening the DM must clear the row count and the tab badge.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail

DEVICE="${1:?usage: maestro-unread.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/unread"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-unread-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-unreadpeer-$STAMP@${BASE#*@}"

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
(cd "$REPO" && cargo build -q -p pollis-tui --example unread_peer)
PEER_BIN="$REPO/target/debug/examples/unread_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export UNREAD_PEER_SIGNAL_DIR="$PEER_DIR/signals"
mkdir -p "$UNREAD_PEER_SIGNAL_DIR"
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

echo "==> 1/3 device opens a DM with the peer, then goes to the background"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-open-dm.yaml"
await_peer PEER_READY 300

echo "==> peer sends while the app is in the background"
touch "$UNREAD_PEER_SIGNAL_DIR/go-1"
await_peer PEER_SENT_1 120

echo "==> 2/3 device resumes: the badge and the row count show the two messages"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/2-resume.yaml"

echo "==> peer sends while the app is closed"
touch "$UNREAD_PEER_SIGNAL_DIR/go-2"
await_peer PEER_SENT_2 120
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""

echo "==> 3/3 device cold-starts: the count survived and grew; opening the DM clears it"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/3-cold-start.yaml"
echo "==> unread passed"
