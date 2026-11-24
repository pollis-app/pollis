#!/usr/bin/env bash
# Display names arrive with the rows that list people (#2085), on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-display-names.sh <device-id>
#
# The peer is `pollis-tui`'s `profile_peer` example (a test-harness build that never ships),
# run with a display name and a DM: it signs up, sets the display name, creates a group,
# invites the device's fresh account by email, waits for it to join, asks to message it and
# writes in that DM. Part 1 checks the member list and the message request name the peer by
# display name on the frame the row appears; part 2 accepts, opens the DM and checks the
# peer's message and the title use the display name; then the peer changes its display name
# and handle, and part 3 sees both follow in the open DM with no relaunch.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled in, and
# POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-display-names.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/display-names"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-dsn-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-dsnpeer-$STAMP@${BASE#*@}"
GROUP_NAME="Names $STAMP"
PEER_NAME="Display Peer"
NEW_HANDLE="renamed-$STAMP"
NEW_NAME="Renamed Display"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer only sends text; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"
export POLLIS_GROUP_NAME="$GROUP_NAME"
export POLLIS_PEER_NAME="$PEER_NAME"
export POLLIS_DM_PHONE=1
export POLLIS_NEW_HANDLE="$NEW_HANDLE"
export POLLIS_NEW_NAME="$NEW_NAME"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example profile_peer)
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
PEER_BIN="$TARGET_DIR/debug/examples/profile_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export POLLIS_RENAME_TRIGGER="$PEER_DIR/rename"
# Set NAMES_KEEP_LOGS=<dir> to keep the peer's output for a failed run.
trap 'kill "${PEER_PID:-}" 2>/dev/null || true; [ -n "${NAMES_KEEP_LOGS:-}" ] && cp "$PEER_DIR"/peer.* "$NAMES_KEEP_LOGS"/ 2>/dev/null; rm -rf "$PEER_DIR"' EXIT

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

echo "==> the peer signs up as \"$PEER_NAME\" and creates \"$GROUP_NAME\""
POLLIS_DATA_DIR="$PEER_DIR/peer" "$PEER_BIN" admin "$PEER_EMAIL" "$PRIMARY_EMAIL" \
  >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_ID= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
PEER_ID="$(sed -n 's/^PEER_ID=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

G=(-e GROUP_NAME="$GROUP_NAME" -e MAESTRO_PEER_HANDLE="$HANDLE" -e MAESTRO_PEER_ID="$PEER_ID"
   -e PEER_NAME="$PEER_NAME" -e NEW_HANDLE="$NEW_HANDLE" -e NEW_NAME="$NEW_NAME")

echo "==> 1/3 device joins; the member list and the message request name the peer"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  "$FLOWS/1-names-with-rows.yaml"
await_peer PEER_READY 300

echo "==> 2/3 device opens the DM; the peer's message and the title use the display name"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/2-dm-labels.yaml"

echo "==> the peer renames itself to $NEW_HANDLE / $NEW_NAME"
sleep 10
touch "$POLLIS_RENAME_TRIGGER"
await_peer PEER_RENAMED 60

echo "==> 3/3 device, DM still open, sees the new display name with no relaunch"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/3-dm-rename.yaml"

wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
echo "==> display names passed"
