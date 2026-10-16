#!/usr/bin/env bash
# QR sign-in between a phone and a desktop, both directions, on ONE device plus
# a dev-DS peer playing the desktop on the desktop's own core commands
# (`pollis-core`'s `enroll_peer` example, built without the OS keychain so it
# never raises a Keychain prompt).
#
#   mobile/scripts/maestro-qr-scan.sh <device-id>
#
# 1. #1361 — the desktop is NOT signed in and shows its code ("Sign in with
#    another device"); the signed-in phone scans it from Security → Scan to
#    sign in a device, enters its PIN, and the desktop must enroll — no email,
#    no paste, no Approve.
# 2. #1207 — the other way: a signed-in desktop shows a v1 code under Link a
#    new device; a NEW phone signs in with it and the desktop approves.
#
# A simulator camera cannot be pointed at a screen, so each scan is entered as
# text, through the same handler the camera calls.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset).
set -euo pipefail

DEVICE="${1:?usage: maestro-qr-scan.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
QR="$HERE/.maestro/two-client/qr-link"
PROMPT="$HERE/.maestro/two-client/approval-prompt"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)-$$"
EMAIL="${BASE%@*}-scan-$STAMP@${BASE#*@}"
EMAIL2="${BASE%@*}-scanned-$STAMP@${BASE#*@}"

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

start_peer() {
  mkdir -p "$WORK/$1"
  POLLIS_DATA_DIR="$WORK/$1/data" "$PEER_BIN" "$2" "$3" >"$WORK/$1/out" 2>"$WORK/$1/err" &
  PIDS+=("$!")
  echo "$!" >"$WORK/$1/pid"
}

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

echo "==> 1/5 phone signs up ($EMAIL)"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$PROMPT/1-phone-signup.yaml"

echo "==> 2/5 a desktop that is not signed in shows its code"
start_peer desk offer -
await_peer desk PEER_OFFER= 60
PAYLOAD="$(sed -n 's/^PEER_OFFER=//p' "$WORK/desk/out" | tail -1)"
echo "    code: ${PAYLOAD:0:28}…"

echo "==> 3/5 the phone scans it and confirms with its PIN; the desktop signs in and enrolls"
maestro --device "$DEVICE" test -e PAYLOAD="$PAYLOAD" "$QR/3-existing-scan.yaml"
await_peer desk PEER_ENROLLED 120

echo "==> 4/5 the other way: a signed-in desktop ($EMAIL2) shows a v1 code; a NEW phone signs in with it"
maestro --device "$DEVICE" test "$QR/v1-1-new-open-manual.yaml"
start_peer owner show "$EMAIL2"
await_peer owner PEER_LINK= 90
LINKCODE="$(sed -n 's/^PEER_LINK=//p' "$WORK/owner/out" | tail -1)"
maestro --device "$DEVICE" test -e PAYLOAD="$LINKCODE" "$QR/v1-2-new-paste-code.yaml"
await_peer owner PEER_APPROVED 120

echo "==> 5/5 the phone finishes enrolling"
maestro --device "$DEVICE" test "$PROMPT/7-phone-finishes.yaml"
echo "==> QR scan passed"
