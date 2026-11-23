#!/usr/bin/env bash
# A profile change elsewhere (#2084) on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-profile-change.sh <device-id>
#
# The peer is `pollis-tui`'s `profile_peer` example (a test-harness build that never
# ships). It signs up, creates a group, invites the device's fresh account by email,
# waits for it to accept and posts a message. Part 1 reads that message under the
# peer's handle; the script then has the peer change its handle and display name, and
# part 2, with the app still open on the channel, sees the old message's sender label
# and the member list follow without a relaunch. That needs the dev DS to send
# `profile_changed`.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled in,
# and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler `dev_personal` when
# unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-profile-change.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/profile-change"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-prf-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-prfpeer-$STAMP@${BASE#*@}"
GROUP_NAME="Renames $STAMP"
NEW_HANDLE="renamed-$STAMP"
NEW_NAME="Renamed Peer"

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
(cd "$REPO" && cargo build -q -p pollis-tui --example profile_peer)
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
PEER_BIN="$TARGET_DIR/debug/examples/profile_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export POLLIS_GROUP_NAME="$GROUP_NAME"
export POLLIS_RENAME_TRIGGER="$PEER_DIR/rename"
export POLLIS_NEW_HANDLE="$NEW_HANDLE"
export POLLIS_NEW_NAME="$NEW_NAME"
# Set PROFILE_KEEP_LOGS=<dir> to keep the peer's output for a failed run.
trap 'kill "${PEER_PID:-}" 2>/dev/null || true; [ -n "${PROFILE_KEEP_LOGS:-}" ] && cp "$PEER_DIR"/peer.* "$PROFILE_KEEP_LOGS"/ 2>/dev/null; rm -rf "$PEER_DIR"' EXIT

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

echo "==> the peer signs up and creates \"$GROUP_NAME\""
POLLIS_DATA_DIR="$PEER_DIR/peer" "$PEER_BIN" admin "$PEER_EMAIL" "$PRIMARY_EMAIL" \
  >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_ID= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
PEER_ID="$(sed -n 's/^PEER_ID=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

G=(-e GROUP_NAME="$GROUP_NAME" -e MAESTRO_PEER_HANDLE="$HANDLE" -e MAESTRO_PEER_ID="$PEER_ID"
   -e NEW_HANDLE="$NEW_HANDLE" -e NEW_NAME="$NEW_NAME")

echo "==> 1/2 device accepts and reads the peer's message under its handle"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  "$FLOWS/1-see-old-name.yaml"
await_peer PEER_READY 300

echo "==> the peer renames itself to $NEW_HANDLE / $NEW_NAME"
touch "$POLLIS_RENAME_TRIGGER"
await_peer PEER_RENAMED 60

echo "==> 2/2 device, still open, sees the new name with no relaunch"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/2-see-new-name.yaml"

wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
echo "==> profile change passed"
