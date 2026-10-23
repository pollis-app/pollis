#!/usr/bin/env bash
# A group created on one of a user's devices shows on their other devices
# without a refresh, both ways, on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-own-groups.sh <device-id>
#
# The peer is `pollis-tui`'s `own_groups_peer` example (a test-harness build
# that never ships), standing in for the user's desktop: it signs up a fresh
# account as its first device and prints the recovery key. Part 1 enrolls the
# device under test into that account. Part 2: the phone creates a group, which
# must show on the phone, and posts in it; the peer must read the post. Then
# the peer creates a group, and part 3 asserts the phone shows it while sitting
# untouched on the Groups tab.
#
# Needs: a Release build against the dev DS (with its LiveKit URL) on the
# device, .maestro/.env filled in, and POLLIS_DEV_ACCESS_KEY for the dev gate
# (read from Doppler `dev_personal` when unset).
set -euo pipefail

DEVICE="${1:?usage: maestro-own-groups.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/own-groups"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_RECOVERY_KEY=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
EMAIL="${BASE%@*}-owngroups-$STAMP@${BASE#*@}"
PHONE_GROUP="Phone $STAMP"
export POLLIS_PEER_GROUP_NAME="Peer $STAMP"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer sends no media; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example own_groups_peer)
PEER_BIN="$TARGET_DIR/debug/examples/own_groups_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export POLLIS_OWN_GROUPS_GO="$PEER_DIR/go"
on_exit() {
  local status=$?
  kill "${PEER_PID:-}" 2>/dev/null || true
  if [ "$status" -ne 0 ]; then
    echo "---- peer stderr ----" >&2
    tail -50 "$PEER_DIR/peer.err" >&2 || true
  fi
  rm -rf "$PEER_DIR"
}
trap on_exit EXIT

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

echo "==> peer signs up as the first device"
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_RECOVERY_KEY= 60
KEY="$(sed -n 's/^PEER_RECOVERY_KEY=//p' "$PEER_OUT")"

echo "==> 1/3 device joins the account with the recovery key"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" \
  -e MAESTRO_RECOVERY_KEY="$KEY" "$FLOWS/1-enroll.yaml"
await_peer PEER_SAW_PHONE 120

echo "==> 2/3 device creates a group and posts; the peer must read it"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PHONE_GROUP="$PHONE_GROUP" \
  "$FLOWS/2-phone-creates.yaml"
await_peer PEER_READ_PHONE_MESSAGE 180

echo "==> peer creates a group"
touch "$POLLIS_OWN_GROUPS_GO"
await_peer PEER_CREATED_GROUP= 120
PEER_GROUP_ID="$(sed -n 's/^PEER_CREATED_GROUP=//p' "$PEER_OUT")"

echo "==> 3/3 device must show the peer's group without being touched"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_GROUP_ID="$PEER_GROUP_ID" \
  "$FLOWS/3-peer-group-live.yaml"
echo "==> own-groups passed"
