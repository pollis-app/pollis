#!/usr/bin/env bash
# Tab bar badges for pending message requests and group invites (#1561) on ONE
# device plus dev-DS peers.
#
#   mobile/scripts/maestro-pending-badges.sh <device-id>
#
# The peers are `pollis-tui`'s `pending_badge_peer` example (a test-harness
# build that never ships), each a fresh account. After the device signs up
# with no badge showing, two strangers open DMs with it (two message requests)
# and a third creates two groups and invites it to both (two group invites).
# Without the device opening Direct or Groups, `badge-direct` and
# `badge-groups` must both read 2. Then: accept one request (1), block the
# other sender (gone); decline one invite (1), accept the other (gone).
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-pending-badges.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/pending-badges"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-badge-$STAMP@${BASE#*@}"
REQ_A_EMAIL="${BASE%@*}-badgereqa-$STAMP@${BASE#*@}"
REQ_B_EMAIL="${BASE%@*}-badgereqb-$STAMP@${BASE#*@}"
INVITER_EMAIL="${BASE%@*}-badgeinv-$STAMP@${BASE#*@}"

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
export POLLIS_GROUP_NAMES="Badge A $STAMP,Badge B $STAMP"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example pending_badge_peer)
PEER_BIN="$REPO/target/debug/examples/pending_badge_peer"
PEER_DIR="$(mktemp -d)"
trap 'rm -rf "$PEER_DIR"' EXIT

# Run one peer to completion; fail with its stderr if it does not print $1.
run_peer() {
  local name="$1" want="$2"
  shift 2
  if ! POLLIS_DATA_DIR="$PEER_DIR/$name" "$PEER_BIN" "$@" >"$PEER_DIR/$name.out" 2>"$PEER_DIR/$name.err" \
    || ! grep -q "^$want" "$PEER_DIR/$name.out"; then
    echo "peer $name did not print $want" >&2
    cat "$PEER_DIR/$name.err" >&2
    exit 1
  fi
}

echo "==> 1/3 device signs up: no badge on Direct or Groups"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  "$FLOWS/1-signup.yaml"

echo "==> two strangers send message requests; a third invites the device to two groups"
run_peer req-a PEER_REQUESTED request "$REQ_A_EMAIL" "$PRIMARY_EMAIL"
run_peer req-b PEER_REQUESTED request "$REQ_B_EMAIL" "$PRIMARY_EMAIL"
run_peer inviter PEER_INVITED invite "$INVITER_EMAIL" "$PRIMARY_EMAIL"

echo "==> 2/3 both badges read 2 untouched; accept one request, block the other"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/2-requests.yaml"

echo "==> 3/3 decline one invite, accept the other"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/3-invites.yaml"
echo "==> pending-badges passed"
