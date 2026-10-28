#!/usr/bin/env bash
# A new device's approval request reaches the phone by itself (#1360), on ONE
# device plus a dev-DS peer, in both directions.
#
#   mobile/scripts/maestro-approval-prompt.sh <device-id>
#
# The peer is `pollis-core`'s `enroll_peer` example: the desktop's half of
# device linking, on the same core commands the desktop app calls, built
# without the OS keychain so it never raises a Keychain prompt.
#
#   desktop → phone: the phone signs up; three peer "desktops" then each ask to
#   be approved — while the app is open (the live nudge), while it is closed
#   (surfaced on reopen + unlock), and while it is in the background (surfaced
#   on return, and rejected). The flows never open Self → Security.
#   phone → desktop: a peer signs up a second account first; the phone signs in
#   to it as a new device, its code is read off the screen and handed to the
#   peer, which approves with the desktop's approve call.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-approval-prompt.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/approval-prompt"
LINK="$HERE/.maestro/two-client/device-link"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
# Seconds plus this shell's pid: two devices started together must not share
# an account.
STAMP="$(date +%Y%m%d%H%M%S)-$$"
EMAIL="${BASE%@*}-approve-$STAMP@${BASE#*@}"
EMAIL2="${BASE%@*}-approved-$STAMP@${BASE#*@}"

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
(cd "$REPO" && cargo build -q -p pollis-core --no-default-features --example enroll_peer)
PEER_BIN="$REPO/target/debug/examples/enroll_peer"
WORK="$(mktemp -d)"
PIDS=()
trap 'for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; rm -rf "$WORK"' EXIT

# start_peer <name> <mode> <email> — runs a peer with its own data dir.
start_peer() {
  mkdir -p "$WORK/$1"
  POLLIS_DATA_DIR="$WORK/$1/data" POLLIS_PEER_CODE_FILE="$WORK/$1/code" \
    "$PEER_BIN" "$2" "$3" >"$WORK/$1/out" 2>"$WORK/$1/err" &
  PIDS+=("$!")
  echo "$!" >"$WORK/$1/pid"
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
  cat "$WORK/$1/err" >&2
  exit 1
}

peer_code() { sed -n 's/^PEER_CODE=//p' "$WORK/$1/out"; }

echo "==> 1/7 phone signs up ($EMAIL)"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/1-phone-signup.yaml"

echo "==> 2/7 a desktop asks to be approved while the phone is open"
start_peer live new "$EMAIL"
await_peer live PEER_CODE= 90
maestro --device "$DEVICE" test -e CODE="$(peer_code live)" "$FLOWS/2-phone-approves-live.yaml"
await_peer live PEER_ENROLLED 120

echo "==> 3/7 a desktop asks while the app is closed; reopening surfaces it"
maestro --device "$DEVICE" test "$FLOWS/3-phone-closes.yaml"
start_peer closed new "$EMAIL"
await_peer closed PEER_CODE= 90
maestro --device "$DEVICE" test -e CODE="$(peer_code closed)" "$FLOWS/4-phone-reopens-approves.yaml"
await_peer closed PEER_ENROLLED 120

echo "==> 4/7 a desktop asks while the app is in the background; resuming surfaces it"
maestro --device "$DEVICE" test "$FLOWS/5-phone-backgrounds.yaml"
start_peer away new "$EMAIL"
await_peer away PEER_CODE= 90
# Long enough that the live nudge, if the connection were still up, has come
# and gone while the app was away.
sleep 5
maestro --device "$DEVICE" test "$FLOWS/6-phone-resumes-rejects.yaml"
echo "==> 5/7 the peer sees the rejection"
await_peer away PEER_REJECTED 120

echo "==> 6/7 phone → desktop: a desktop owns $EMAIL2; the phone signs in to it as a new device"
start_peer owner approver "$EMAIL2"
await_peer owner PEER_SIGNED_UP 90
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL2" "$LINK/2-new-request.yaml"
await_peer owner PEER_SAW_REQUEST= 60
# Read the code off the phone (the same parse as maestro-device-link.sh).
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

echo "==> 7/7 phone finishes enrolling"
maestro --device "$DEVICE" test "$FLOWS/7-phone-finishes.yaml"
echo "==> approval prompt passed"
