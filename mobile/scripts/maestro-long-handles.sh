#!/usr/bin/env bash
# Long handles in Members, Message requests and Direct, and the sign-out sheet
# (#1971), on ONE device plus dev-DS peers.
#
#   mobile/scripts/maestro-long-handles.sh <device-id>
#
# The device signs up with a long email, so the DS gives it a 32-character
# handle (the longest it allows). The peers are `pollis-tui`'s `pending_badge_peer` example (a test-harness build
# that never ships), renamed to long handles: stranger A (handle only) and
# stranger B (handle plus a display name) ask to message the device, and an
# owner creates a group and invites it. The device then checks Message
# requests, Direct and the group's Members (`2-lists.yaml`), and signs out
# through the sheet (`3-sign-out.yaml`). Screenshots go to SHOTS_DIR when set.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). The peer builds into CARGO_TARGET_DIR when set.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-long-handles.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/long-handles"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
# Ten digits keep each handle at exactly 32 characters (22 + 10).
SHORT="$(date +%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-longh-$STAMP@${BASE#*@}"
REQ_A_EMAIL="${BASE%@*}-longha-$STAMP@${BASE#*@}"
REQ_B_EMAIL="${BASE%@*}-longhb-$STAMP@${BASE#*@}"
OWNER_EMAIL="${BASE%@*}-longho-$STAMP@${BASE#*@}"
REQ_A_HANDLE="longhandle-tests-reqa-$SHORT"
REQ_B_HANDLE="longhandle-tests-reqb-$SHORT"
OWNER_HANDLE="longhandle-tests-ownr-$SHORT"
REQ_B_NAME="Ada Lovelace"
GROUP_NAME="Long $STAMP"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peers send no media and join no call; these must be set but are never
# contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"
export POLLIS_GROUP_NAMES="$GROUP_NAME"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example pending_badge_peer)
PEER_BIN="${CARGO_TARGET_DIR:-$REPO/target}/debug/examples/pending_badge_peer"
PEER_DIR="$(mktemp -d)"
trap 'rm -rf "$PEER_DIR"' EXIT

# Run one peer to completion; fail with its stderr if it does not print $2.
run_peer() {
  local name="$1" want="$2" username="$3" display="$4"
  shift 4
  if ! POLLIS_DATA_DIR="$PEER_DIR/$name" POLLIS_PEER_USERNAME="$username" \
    POLLIS_PEER_DISPLAY_NAME="$display" \
    "$PEER_BIN" "$@" >"$PEER_DIR/$name.out" 2>"$PEER_DIR/$name.err" \
    || ! grep -q "^$want" "$PEER_DIR/$name.out"; then
    echo "peer $name did not print $want" >&2
    cat "$PEER_DIR/$name.err" >&2
    exit 1
  fi
}

DEBUG_ARGS=()
if [ -n "${SHOTS_DIR:-}" ]; then
  mkdir -p "$SHOTS_DIR"
  DEBUG_ARGS=(--test-output-dir "$SHOTS_DIR")
fi
FLOW_ENV=(-e REQ_A_HANDLE="$REQ_A_HANDLE"
  -e REQ_B_NAME="$REQ_B_NAME" -e OWNER_HANDLE="$OWNER_HANDLE" -e GROUP_NAME="$GROUP_NAME")

echo "==> 1/3 device signs up with a 32-character handle"
maestro --device "$DEVICE" test "${DEBUG_ARGS[@]}" "${ENV_ARGS[@]}" \
  -e MAESTRO_EMAIL="$PRIMARY_EMAIL" "${FLOW_ENV[@]}" "$FLOWS/1-signup.yaml"

echo "==> two long-handled strangers ask to message the device; a third invites it"
run_peer req-a PEER_REQUESTED "$REQ_A_HANDLE" "" request "$REQ_A_EMAIL" "$PRIMARY_EMAIL"
run_peer req-b PEER_REQUESTED "$REQ_B_HANDLE" "$REQ_B_NAME" request "$REQ_B_EMAIL" "$PRIMARY_EMAIL"
run_peer owner PEER_INVITED "$OWNER_HANDLE" "" invite "$OWNER_EMAIL" "$PRIMARY_EMAIL"

echo "==> 2/3 Message requests, Direct and Members"
maestro --device "$DEVICE" test "${DEBUG_ARGS[@]}" "${ENV_ARGS[@]}" "${FLOW_ENV[@]}" \
  "$FLOWS/2-lists.yaml"

echo "==> 3/3 sign out through the sheet"
maestro --device "$DEVICE" test "${DEBUG_ARGS[@]}" "${ENV_ARGS[@]}" "${FLOW_ENV[@]}" \
  "$FLOWS/3-sign-out.yaml"
echo "==> long-handles passed"
