#!/usr/bin/env bash
# Copy message history between one account's own devices (#1735), on ONE phone
# plus scripted desktop peers on the dev DS, in both directions.
#
#   mobile/scripts/maestro-history-transfer.sh <device-id>
#
# The peers are `pollis-core`'s `enroll_peer` example (the desktop's own core
# commands, built without the OS keychain):
#
#   phone = receiver: an "owner" peer signs up, seeds SEEDED messages in a group
#   (`--seed-history`), and approves the phone's sign-in code. The phone is then
#   offered the owner's history, copies it, and the owner approves the request
#   with its PIN and sends (`--transfer-source`). The phone must show the seeded
#   messages (oldest and newest, each exactly once, no undecryptable rows).
#   phone = source: a second peer device asks to join; the phone approves its
#   code, then the peer asks for the phone's history (`--transfer-receiver`). The
#   phone's approval page opens on its own, takes the PIN and shows the send to
#   the end; the peer must report every seeded row received.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). Honours CARGO_TARGET_DIR for the peer build.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-history-transfer.sh <device-id>}"
SEEDED="${SEEDED:-30}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/history-transfer"
LINK="$HERE/.maestro/two-client/device-link"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)-$$"
EMAIL="${BASE%@*}-history-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The peer sends no media; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

TARGET="${CARGO_TARGET_DIR:-$REPO/target}"
echo "==> building the peer"
(cd "$REPO" && CARGO_TARGET_DIR="$TARGET" cargo build -q -p pollis-core --no-default-features --example enroll_peer)
PEER_BIN="$TARGET/debug/examples/enroll_peer"
WORK="$(mktemp -d)"
PIDS=()
trap 'for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$WORK"' EXIT

# start_peer <name> <args…> — runs a peer with its own data dir.
start_peer() {
  local name="$1"
  shift
  mkdir -p "$WORK/$name"
  POLLIS_DATA_DIR="$WORK/$name/data" POLLIS_PEER_CODE_FILE="$WORK/$name/code" \
    "$PEER_BIN" "$@" >"$WORK/$name/out" 2>"$WORK/$name/err" &
  PIDS+=("$!")
  echo "$!" >"$WORK/$name/pid"
}

# await_peer <name> <line-prefix> <seconds>
await_peer() {
  local pid
  pid="$(cat "$WORK/$1/pid")"
  for _ in $(seq 1 "$3"); do
    grep -q "^$2" "$WORK/$1/out" 2>/dev/null && return 0
    if ! kill -0 "$pid" 2>/dev/null; then
      grep -q "^$2" "$WORK/$1/out" 2>/dev/null && return 0
      echo "peer $1 exited before printing $2" >&2
      cat "$WORK/$1/out" "$WORK/$1/err" >&2
      exit 1
    fi
    sleep 1
  done
  echo "peer $1 never printed $2" >&2
  cat "$WORK/$1/out" "$WORK/$1/err" >&2
  exit 1
}

peer_line() { sed -n "s/^$2=//p" "$WORK/$1/out" | tail -1; }

# expect_rows <name> — the peer's PEER_TRANSFER_DONE must be `completed` with every seeded row.
expect_rows() {
  local done
  done="$(peer_line "$1" PEER_TRANSFER_DONE)"
  echo "    $1: $done"
  case "$done" in completed\ *) ;; *) echo "peer $1's transfer did not complete: $done" >&2; exit 1;; esac
  local rows
  rows="$(printf '%s' "$done" | sed -n 's/.*rows=\([0-9]*\).*/\1/p')"
  if [ -z "$rows" ] || [ "$rows" -lt "$SEEDED" ]; then
    echo "peer $1 moved $rows rows, expected at least $SEEDED" >&2
    exit 1
  fi
}

echo "==> 1/6 the owner peer signs up $EMAIL and seeds $SEEDED messages"
start_peer owner approver "$EMAIL" --seed-history "$SEEDED" --transfer-source
await_peer owner PEER_SEEDED= 180

echo "==> 2/6 the phone signs in to it as a new device; its code goes to the owner"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$LINK/2-new-request.yaml"
await_peer owner PEER_SAW_REQUEST= 60
if [[ "$DEVICE" == emulator-* || "$DEVICE" == *:* ]]; then
  adb -s "$DEVICE" shell uiautomator dump /sdcard/approve-ui.xml >/dev/null
  TREE="$(adb -s "$DEVICE" exec-out cat /sdcard/approve-ui.xml)"
else
  TREE="$(maestro --device "$DEVICE" hierarchy 2>/dev/null)"
fi
CODE="$(printf '%s' "$TREE" | grep -oE '(text|accessibilityText)"? ?[=:] ?"[0-9A-HJKMNP-TV-Z]{8}"' | grep -oE '"[0-9A-HJKMNP-TV-Z]{8}"' | tr -d '"' | head -1)"
[ -n "$CODE" ] || { echo "could not read the verification code from the phone" >&2; exit 1; }
printf '%s' "$CODE" >"$WORK/owner/code"
await_peer owner PEER_APPROVED 60

echo "==> 3/6 phone = receiver: offered the owner's history, copies it, sees it"
maestro --device "$DEVICE" test -e SEEDED="$(printf '%02d' "$SEEDED")" "$FLOWS/1-receiver-copies.yaml"
await_peer owner PEER_TRANSFER_DONE= 120
expect_rows owner

echo "==> 4/6 a second peer device asks to join; the phone approves its code"
start_peer second new "$EMAIL" --transfer-receiver
await_peer second PEER_CODE= 90
maestro --device "$DEVICE" test -e CODE="$(peer_line second PEER_CODE)" "$FLOWS/2-phone-approves-device.yaml"
await_peer second PEER_ENROLLED 120

echo "==> 5/6 phone = source: the peer asks for the phone's history; the phone approves with its PIN"
await_peer second PEER_TRANSFER_REQUESTED= 60
maestro --device "$DEVICE" test "$FLOWS/3-source-approves.yaml"
await_peer second PEER_TRANSFER_DONE= 240
expect_rows second

echo "==> 6/6 Security shows the approval and the picker"
maestro --device "$DEVICE" test "$FLOWS/4-security-shows-events.yaml"
echo "==> history transfer passed"
