#!/usr/bin/env bash
# Link previews across platforms (#1987) on ONE device plus a dev-DS peer.
#
#   mobile/scripts/maestro-link-previews.sh <device-id>
#
# The peer is `pollis-tui`'s `link_preview_peer` example (a test-harness build
# that never ships): the same pollis-core the desktop app runs. It signs up a
# fresh account, accepts the DM the device opens, builds a preview of
# https://github.com/ itself and sends it. Part 2 asserts the device draws that
# card, then sends its own preview, which the peer must read back before it
# moves on. Then edit and delete both ways: the peer edits its message to
# another link (its replaced thumbnail is released on the DS and must stop
# downloading), part 3 asserts the device shows the new card; the peer deletes
# for everyone (the new thumbnail goes too), and part 4 asserts the card is gone,
# then edits the device's own message to another link and deletes it, which the
# peer must read back before it exits 0. The single-device half is
# .maestro/flows/link-previews.yaml.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-link-previews.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/link-previews"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-lp-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-lppeer-$STAMP@${BASE#*@}"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer uploads its thumbnail: the dev R2 origin the app is built with (mobile/.env;
# no secret, uploads are presigned by the DS, which refuses any other origin).
if [ -z "${R2_S3_ENDPOINT:-}" ] && [ -f "$HERE/.env" ]; then
  R2_S3_ENDPOINT="$(sed -n 's/^EXPO_PUBLIC_R2_ENDPOINT=//p' "$HERE/.env" | head -1)"
  R2_PUBLIC_URL="$(sed -n 's/^EXPO_PUBLIC_R2_PUBLIC_URL=//p' "$HERE/.env" | head -1)"
fi
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:?R2_S3_ENDPOINT (dev) is required}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:?R2_PUBLIC_URL (dev) is required}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example link_preview_peer)
PEER_BIN="${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/link_preview_peer"
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
POLLIS_PEER_GO_DELETE="$PEER_DIR/go-delete" POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

echo "==> 1/4 device opens a DM with the peer"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-open-dm.yaml"

echo "==> peer builds its preview and sends"
await_peer PEER_SENT 300
grep '^PEER_BUILT' "$PEER_OUT"

echo "==> 2/4 device draws the peer's card, then sends its own"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/2-cards.yaml"

echo "==> peer reads the device's preview, edits its own message and checks the old thumbnail is released"
await_peer PEER_SAW_DEVICE_PREVIEW 300
await_peer PEER_RELEASED_OLD_THUMB 300
grep -E '^PEER_(SAW|THUMB|EDITED|RELEASED)' "$PEER_OUT"

echo "==> 3/4 device shows the peer's edited card"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/3-peer-edit.yaml"

echo "==> peer deletes for everyone"
touch "$PEER_DIR/go-delete"
await_peer PEER_RELEASED_NEW_THUMB 300
grep -E '^PEER_(DELETED|RELEASED_NEW)' "$PEER_OUT"

echo "==> 4/4 device sees the delete, then edits and deletes its own"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/4-peer-delete.yaml"

echo "==> peer reads the device's edit and delete"
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
grep -E '^PEER_SAW_DEVICE_(EDIT|DELETE)' "$PEER_OUT"
echo "==> link previews passed"
