#!/usr/bin/env bash
# A device revoked while it was not listening signs itself out (#1338), on ONE
# device plus a dev-DS peer.
#
#   mobile/scripts/maestro-device-revoked.sh <device-id>
#
# The peer is `pollis-tui`'s `revoke_peer` example (a test-harness build that
# never ships): it signs up a fresh account as its first device and prints the
# recovery key. Part 1 enrolls the device under test into that account with the
# key, then closes the app. The peer revokes the device while the app is closed
# (so no live nudge can reach it). Part 2 reopens and unlocks the app and
# asserts it lands on sign-in with the "removed from your account" notice.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-device-revoked.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/device-revoked"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_RECOVERY_KEY=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
EMAIL="${BASE%@*}-revoked-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer sends no media; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example revoke_peer)
PEER_BIN="$REPO/target/debug/examples/revoke_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export POLLIS_REVOKE_GO="$PEER_DIR/go"
trap 'kill "${PEER_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

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

echo "==> 1/2 device joins the account with the recovery key, then the app closes"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" \
  -e MAESTRO_RECOVERY_KEY="$KEY" "$FLOWS/1-enroll.yaml"
await_peer PEER_SAW_PHONE 120

echo "==> peer revokes the device while its app is closed"
touch "$POLLIS_REVOKE_GO"
await_peer PEER_REVOKED 120

echo "==> 2/2 device reopens, unlocks, and must land on sign-in saying why"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/2-revoked.yaml"
echo "==> device-revoked passed"
