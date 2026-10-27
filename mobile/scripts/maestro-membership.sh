#!/usr/bin/env bash
# Group membership without timeline lines (#1506) on ONE device plus dev-DS
# peers.
#
#   mobile/scripts/maestro-membership.sh <device-id>
#
# The peers are `pollis-tui`'s `membership_peer` example (a test-harness build
# that never ships). One signs up an account that will stay a PENDING invitee;
# the other creates a group, invites the device's fresh account by email, waits
# for it to accept, invites the pending account and makes the device an admin.
# Part 1 accepts and posts (the device's message carries the "new" tag, no
# "joined" line), then creates a group of its own and posts there untagged (a
# creator is never tagged); part 2 revokes the pending invite from the member
# page's "Invited" section. The peer must see the invite listed and then gone
# before it exits 0.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail

DEVICE="${1:?usage: maestro-membership.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/membership"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-mbr-$STAMP@${BASE#*@}"
ADMIN_EMAIL="${BASE%@*}-mbradmin-$STAMP@${BASE#*@}"
PENDING_EMAIL="${BASE%@*}-mbrpending-$STAMP@${BASE#*@}"
GROUP_NAME="Membership $STAMP"

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
export POLLIS_GROUP_NAME="$GROUP_NAME"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example membership_peer)
PEER_BIN="$REPO/target/debug/examples/membership_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
# Set MEMBERSHIP_KEEP_LOGS=<dir> to keep the peer's output for a failed run.
trap 'kill "${PEER_PID:-}" 2>/dev/null || true; [ -n "${MEMBERSHIP_KEEP_LOGS:-}" ] && cp "$PEER_DIR"/peer.* "$MEMBERSHIP_KEEP_LOGS"/ 2>/dev/null; rm -rf "$PEER_DIR"' EXIT

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

echo "==> the pending invitee signs up"
PENDING_HANDLE="$(POLLIS_DATA_DIR="$PEER_DIR/pending" "$PEER_BIN" signup "$PENDING_EMAIL" \
  | sed -n 's/^PEER_HANDLE=//p')"
echo "    pending handle: $PENDING_HANDLE"

echo "==> the group's creator signs up and creates \"$GROUP_NAME\""
POLLIS_DATA_DIR="$PEER_DIR/admin" "$PEER_BIN" admin "$ADMIN_EMAIL" "$PRIMARY_EMAIL" "$PENDING_EMAIL" \
  >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    creator handle: $HANDLE"

G=(-e GROUP_NAME="$GROUP_NAME" -e MAESTRO_PEER_HANDLE="$HANDLE" -e MAESTRO_PENDING_HANDLE="$PENDING_HANDLE")

echo "==> 1/2 device accepts and posts (tagged new, no joined line); its own group untagged"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  "$FLOWS/1-accept-and-post.yaml"

echo "==> creator invites the pending account, promotes the device"
await_peer PEER_PROMOTED 300

echo "==> 2/2 device revokes the pending invite"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/2-tags-and-revoke.yaml"

echo "==> creator sees the invite gone"
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
grep '^PEER_SAW_REVOKE' "$PEER_OUT"
echo "==> membership passed"
