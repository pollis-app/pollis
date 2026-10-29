#!/usr/bin/env bash
# A second device that never opened the app while a DM was made and used must
# still show every message when it does (#1571), on ONE device plus two dev-DS
# peers.
#
#   mobile/scripts/maestro-device-outside-tree.sh <device-id>
#
# The peers are `pollis-tui`'s `outside_tree_peer` example (a test-harness build
# that never ships):
#   * `owner` signs up an account as its FIRST device and prints the recovery
#     key; part 1 enrolls the device under test into that account with it, then
#     closes the app;
#   * `creator` (another account), once the app is closed, spends the phone's
#     regular KeyPackage pool (five group invites to the account), opens a DM
#     with the account, and exchanges messages with the owner device.
# Part 2 reopens the phone and asserts all three messages are there and no
# placeholder. Before #1571 the phone could be left out of the DM's tree
# entirely and show it empty, with no error.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). The DS must carry #1571 (migration 000042 and the
# last-resort claim). POLLIS_PEER_TARGET overrides the peers' cargo target dir.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-device-outside-tree.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/device-outside-tree"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_RECOVERY_KEY=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
OWNER_EMAIL="${BASE%@*}-outside-$STAMP@${BASE#*@}"
CREATOR_EMAIL="${BASE%@*}-outsidepeer-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peers send no media; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

TARGET="${POLLIS_PEER_TARGET:-$REPO/target}"
echo "==> building the peers"
(cd "$REPO" && CARGO_TARGET_DIR="$TARGET" cargo build -q -p pollis-tui --example outside_tree_peer)
PEER_BIN="$TARGET/debug/examples/outside_tree_peer"
PEER_DIR="$(mktemp -d)"
OWNER_OUT="$PEER_DIR/owner.out"
CREATOR_OUT="$PEER_DIR/creator.out"
export POLLIS_OUTSIDE_TREE_GO="$PEER_DIR/go"
trap 'kill "${OWNER_PID:-}" "${CREATOR_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

await_line() {
  local out="$1" pid="$2" err="$3" want="$4" secs="$5"
  for _ in $(seq 1 "$secs"); do
    grep -q "^$want" "$out" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || { cat "$err" >&2; exit 1; }
    sleep 1
  done
  echo "peer never printed $want" >&2
  cat "$err" >&2
  exit 1
}

echo "==> owner peer signs up as the account's first device"
POLLIS_DATA_DIR="$PEER_DIR/owner" "$PEER_BIN" owner "$OWNER_EMAIL" >"$OWNER_OUT" 2>"$PEER_DIR/owner.err" &
OWNER_PID=$!
await_line "$OWNER_OUT" "$OWNER_PID" "$PEER_DIR/owner.err" PEER_USER_ID= 90
KEY="$(sed -n 's/^PEER_RECOVERY_KEY=//p' "$OWNER_OUT")"
OWNER_ID="$(sed -n 's/^PEER_USER_ID=//p' "$OWNER_OUT")"

echo "==> 1/2 device joins the account with the recovery key, then the app closes"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$OWNER_EMAIL" \
  -e MAESTRO_RECOVERY_KEY="$KEY" "$FLOWS/1-enroll.yaml"
await_line "$OWNER_OUT" "$OWNER_PID" "$PEER_DIR/owner.err" PEER_SAW_PHONE= 120

echo "==> creator peer spends the phone's pool, opens the DM and talks to the owner device"
POLLIS_DATA_DIR="$PEER_DIR/creator" "$PEER_BIN" creator "$CREATOR_EMAIL" "$OWNER_ID" "$OWNER_EMAIL" \
  >"$CREATOR_OUT" 2>"$PEER_DIR/creator.err" &
CREATOR_PID=$!
await_line "$CREATOR_OUT" "$CREATOR_PID" "$PEER_DIR/creator.err" PEER_CREATOR_HANDLE= 90
HANDLE="$(sed -n 's/^PEER_CREATOR_HANDLE=//p' "$CREATOR_OUT")"
touch "$POLLIS_OUTSIDE_TREE_GO"
await_line "$OWNER_OUT" "$OWNER_PID" "$PEER_DIR/owner.err" PEER_REPLIED 300
await_line "$CREATOR_OUT" "$CREATOR_PID" "$PEER_DIR/creator.err" PEER_DONE 300

echo "==> 2/2 device opens the app for the first time since, and the DM"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/2-open-dm.yaml"
echo "==> device-outside-tree passed"
