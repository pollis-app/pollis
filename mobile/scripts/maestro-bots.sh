#!/usr/bin/env bash
# Bots (#1182) on ONE device plus two `pollis-bot` hosts on this machine.
#
#   mobile/scripts/maestro-bots.sh <device-id>
#
# Host 1 starts `pollis-bot pair` with no state and prints its pairing code.
# Part 1 (flows/bots.yaml) signs a fresh admin in, creates a group, is refused
# a wrong code and types the real one: the bot is created and paired. The
# script then posts an alert through the bot's webhook (`pollis-bot post`);
# part 2 sees it in #General with the Bot badge, and the badge on the member
# list. Host 2 pairs as a second host of the same bot; part 3 types its code.
# Part 4 revokes host 2 and deletes the bot, after which host 1 must have
# stopped: its identity is gone.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled
# in, and POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler
# `dev_personal` when unset). Builds `pollis-bot` from this checkout.
#
# The `pollis-bot` command lines are the ones below (BOT_* variables); keep
# them in step with the runtime's CLI and with the command the Bots page shows
# (mobile/lib/bots.ts BOT_PAIR_COMMAND).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-bots.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/bots"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
ADMIN_EMAIL="${BASE%@*}-bots-$STAMP@${BASE#*@}"
GROUP_NAME="Bots $STAMP"
BOT_NAME="Alerts $STAMP"
ALERT_TEXT="disk 91% on nas-$STAMP"

# The dev gate key (#1242). Unset is fine while the gate is in `report` mode.
if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
# The bot only sends text; these must be set but are never contacted.
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

echo "==> building pollis-bot"
(cd "$REPO" && cargo build -q -p pollis-bot)
TARGET_DIR="${CARGO_TARGET_DIR:-$REPO/target}"
BOT_BIN="$TARGET_DIR/debug/pollis-bot"
WORK="$(mktemp -d)"
# Set BOTS_KEEP_LOGS=<dir> to keep both hosts' output for a failed run.
cleanup() {
  kill "${HOST1_PID:-}" "${HOST2_PID:-}" 2>/dev/null || true
  [ -n "${BOTS_KEEP_LOGS:-}" ] && cp "$WORK"/*.out "$WORK"/*.err "$BOTS_KEEP_LOGS"/ 2>/dev/null
  rm -rf "$WORK"
}
trap cleanup EXIT

# Each host is its own installation directory: nothing is shared between them.
BOT_PAIR=(pair)
BOT_ADD_HOST=(pair)
BOT_POST=(post --channel General)

# Wait (up to $3 seconds) for a line matching $2 in host $1's output; fail if it dies first.
await_host() {
  local name="$1" pattern="$2" secs="$3" pid
  pid="$(eval echo "\${${name}_PID}")"
  for _ in $(seq 1 "$secs"); do
    grep -qiE "$pattern" "$WORK/$name.out" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || { echo "$name exited" >&2; cat "$WORK/$name.err" >&2; exit 1; }
    sleep 1
  done
  echo "$name never printed /$pattern/" >&2
  cat "$WORK/$name.out" "$WORK/$name.err" >&2
  exit 1
}

# The eight-character pairing code (enrollment-SAS alphabet) on the host's "code" line.
code_of() {
  grep -iE 'code' "$WORK/$1.out" | grep -oE '\b[0-9A-HJKMNP-TV-Z]{8}\b' | head -1
}

echo "==> host 1 starts with no state and prints its pairing code"
"$BOT_BIN" --data-dir "$WORK/host1" "${BOT_PAIR[@]}" >"$WORK/HOST1.out" 2>"$WORK/HOST1.err" &
HOST1_PID=$!
await_host HOST1 'code' 60
BOT_CODE="$(code_of HOST1)"
[ -n "$BOT_CODE" ] || { echo "no pairing code from host 1" >&2; cat "$WORK/HOST1.out" >&2; exit 1; }
echo "    code: $BOT_CODE"

G=(-e GROUP_NAME="$GROUP_NAME" -e BOT_NAME="$BOT_NAME" -e ALERT_TEXT="$ALERT_TEXT")

echo "==> 1/4 admin creates \"$GROUP_NAME\" and pairs the bot"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$ADMIN_EMAIL" \
  -e BOT_CODE="$BOT_CODE" "$HERE/.maestro/flows/bots.yaml"

echo "==> host 1 picks up the approval and joins the group"
await_host HOST1 'connected' 180

echo "==> the bot posts an alert through its webhook"
"$BOT_BIN" --data-dir "$WORK/host1" "${BOT_POST[@]}" "$ALERT_TEXT"

echo "==> 2/4 the alert carries the Bot badge"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/2-alert.yaml"

echo "==> host 2 asks to join the same bot"
"$BOT_BIN" --data-dir "$WORK/host2" "${BOT_ADD_HOST[@]}" >"$WORK/HOST2.out" 2>"$WORK/HOST2.err" &
HOST2_PID=$!
await_host HOST2 'code' 60
HOST_CODE="$(code_of HOST2)"
[ -n "$HOST_CODE" ] || { echo "no pairing code from host 2" >&2; cat "$WORK/HOST2.out" >&2; exit 1; }
echo "    code: $HOST_CODE"

echo "==> 3/4 admin adds host 2"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e HOST_CODE="$HOST_CODE" "$FLOWS/3-add-host.yaml"
await_host HOST2 'connected' 180

echo "==> 4/4 admin revokes host 2 and deletes the bot"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/4-revoke-delete.yaml"

echo "==> host 1 stops once its bot is deleted"
for _ in $(seq 1 120); do
  kill -0 "$HOST1_PID" 2>/dev/null || break
  sleep 1
done
if kill -0 "$HOST1_PID" 2>/dev/null; then
  echo "host 1 is still running after its bot was deleted" >&2
  exit 1
fi
HOST1_PID=""
echo "==> bots passed"
