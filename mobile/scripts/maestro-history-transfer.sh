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
#   QR link, old device: a signed-out peer shows its code, the phone scans it
#   (typed in) and the new peer asks with via=link; the phone must offer the
#   one-tap approval INLINE on the "Linked" page and send to the end.
#   Kill mid-copy: a fresh account seeded with BIG padded messages (many
#   chunks); the phone copies, is killed once rows arrive, relaunches, and the
#   copy must resume to exactly BIG messages with no duplicate.
#
# Env: SEEDED (30), BIG (3000), BIG_PAD (4000 bytes per message), SLOW_MS (250, the source's
# per-read delay once the copy starts), SLOW_PORT (18790), SKIP_KILL=1,
# ONLY_KILL=1 (step 8 alone), KEEP_WORK=1 (keep the peers' logs; the path is printed).
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). Honours CARGO_TARGET_DIR for the peer build.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-history-transfer.sh <device-id>}"
SEEDED="${SEEDED:-30}"
BIG="${BIG:-3000}"
BIG_PAD="${BIG_PAD:-4000}"
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
EMAIL_BIG="${BASE%@*}-history-big-$STAMP@${BASE#*@}"

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
trap 'for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; if [ "${KEEP_WORK:-0}" = 1 ]; then echo "peer logs: $WORK"; else rm -rf "$WORK"; fi' EXIT

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

# phone_code_to <peer> — read the phone's 8-character enrollment code off the screen into the peer's code file.
phone_code_to() {
  local tree code
  if [[ "$DEVICE" == emulator-* || "$DEVICE" == *:* ]]; then
    adb -s "$DEVICE" shell uiautomator dump /sdcard/approve-ui.xml >/dev/null
    tree="$(adb -s "$DEVICE" exec-out cat /sdcard/approve-ui.xml)"
  else
    tree="$(maestro --device "$DEVICE" hierarchy 2>/dev/null)"
  fi
  code="$(printf '%s' "$tree" | grep -oE '(text|accessibilityText)"? ?[=:] ?"[0-9A-HJKMNP-TV-Z]{8}"' | grep -oE '"[0-9A-HJKMNP-TV-Z]{8}"' | tr -d '"' | head -1)"
  [ -n "$code" ] || { echo "could not read the verification code from the phone" >&2; exit 1; }
  printf '%s' "$code" >"$WORK/$1/code"
}

peer_line() { sed -n "s/^$2=//p" "$WORK/$1/out" | tail -1; }

# expect_rows <name> [min] — the peer's PEER_TRANSFER_DONE must be `completed` with every seeded row.
expect_rows() {
  local min="${2:-$SEEDED}"
  local done
  done="$(peer_line "$1" PEER_TRANSFER_DONE)"
  echo "    $1: $done"
  case "$done" in completed\ *) ;; *) echo "peer $1's transfer did not complete: $done" >&2; exit 1;; esac
  local rows
  rows="$(printf '%s' "$done" | sed -n 's/.*rows=\([0-9]*\).*/\1/p')"
  if [ -z "$rows" ] || [ "$rows" -lt "$min" ]; then
    echo "peer $1 moved $rows rows, expected at least $min" >&2
    exit 1
  fi
}

if [ "${ONLY_KILL:-0}" != 1 ]; then
echo "==> 1/8 the owner peer signs up $EMAIL and seeds $SEEDED messages"
start_peer owner approver "$EMAIL" --seed-history "$SEEDED" --transfer-source
await_peer owner PEER_SEEDED= 180

echo "==> 2/8 the phone signs in to it as a new device; its code goes to the owner"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$LINK/2-new-request.yaml"
await_peer owner PEER_SAW_REQUEST= 60
phone_code_to owner
await_peer owner PEER_APPROVED 60

echo "==> 3/8 phone = receiver: offered the owner's history, copies it, sees it"
maestro --device "$DEVICE" test -e SEEDED="$(printf '%02d' "$SEEDED")" "$FLOWS/1-receiver-copies.yaml"
await_peer owner PEER_TRANSFER_DONE= 120
expect_rows owner

echo "==> 4/8 a second peer device asks to join; the phone approves its code"
start_peer second new "$EMAIL" --transfer-receiver
await_peer second PEER_CODE= 90
maestro --device "$DEVICE" test -e CODE="$(peer_line second PEER_CODE)" "$FLOWS/2-phone-approves-device.yaml"
await_peer second PEER_ENROLLED 120

echo "==> 5/8 phone = source: the peer asks for the phone's history; the phone approves with its PIN"
await_peer second PEER_TRANSFER_REQUESTED= 60
maestro --device "$DEVICE" test "$FLOWS/3-source-approves.yaml"
await_peer second PEER_TRANSFER_DONE= 240
expect_rows second

echo "==> 6/8 Security shows the approval and the picker"
maestro --device "$DEVICE" test "$FLOWS/4-security-shows-events.yaml"
echo "==> 7/8 QR link: the phone links a new device, which asks with via=link; one tap inline"
start_peer qr offer - --transfer-receiver --transfer-via link
await_peer qr PEER_OFFER= 90
maestro --device "$DEVICE" test -e PAYLOAD="$(peer_line qr PEER_OFFER)" "$FLOWS/5-qr-link-inline.yaml"
await_peer qr PEER_TRANSFER_DONE= 240
expect_rows qr

fi

if [ "${SKIP_KILL:-0}" = 1 ]; then
  echo "==> 8/8 skipped (SKIP_KILL=1)"
else
  echo "==> 8/8 kill mid-copy: $EMAIL_BIG seeded with $BIG messages of ~$BIG_PAD bytes"
  # The source goes through scripts/slow-proxy.py, held back only once the copy starts (the
  # switch file), so a fast device cannot finish before the kill lands mid-transfer.
  SLOW_PORT="${SLOW_PORT:-18790}"
  SLOW_SWITCH="$WORK/slow-on"
  python3 "$HERE/scripts/slow-proxy.py" "$SLOW_PORT" "$SLOW_SWITCH" "${SLOW_MS:-250}" >"$WORK/slow-proxy.out" 2>&1 &
  PIDS+=("$!")
  for _ in $(seq 1 50); do
    grep -q "^slow-proxy on" "$WORK/slow-proxy.out" 2>/dev/null && break
    sleep 0.2
  done
  grep -q "^slow-proxy on" "$WORK/slow-proxy.out" || { echo "slow-proxy did not start" >&2; cat "$WORK/slow-proxy.out" >&2; exit 1; }
  HTTP_PROXY="http://127.0.0.1:$SLOW_PORT" HTTPS_PROXY="http://127.0.0.1:$SLOW_PORT" \
    http_proxy="http://127.0.0.1:$SLOW_PORT" https_proxy="http://127.0.0.1:$SLOW_PORT" NO_PROXY= no_proxy= \
    start_peer big approver "$EMAIL_BIG" --seed-history "$BIG" --seed-pad "$BIG_PAD" --transfer-source
  await_peer big PEER_SEEDED= 1800
  maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL_BIG" "$LINK/2-new-request.yaml"
  await_peer big PEER_SAW_REQUEST= 60
  phone_code_to big
  await_peer big PEER_APPROVED 60
  touch "$SLOW_SWITCH"
  maestro --device "$DEVICE" test -e SEEDED="$BIG" "$FLOWS/6-receiver-kill-resume.yaml"
  await_peer big PEER_TRANSFER_DONE= 600
  expect_rows big "$BIG"
fi
echo "==> history transfer passed"
