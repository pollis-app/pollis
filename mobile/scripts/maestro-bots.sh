#!/usr/bin/env bash
# Bots (#1182) on ONE device plus two `pollis-bot` hosts on this machine.
#
#   mobile/scripts/maestro-bots.sh <device-id>
#
# Host 1 starts `pollis-bot pair` with no state and prints its pairing code.
# Part 1 (flows/bots.yaml) signs a fresh admin in, creates a group, is refused
# a wrong code and types the real one: the bot is created and `pair` exits.
# Host 1 then runs (`pollis-bot run --webhook`) and the script posts an alert
# through the bot's webhook (`pollis-bot post`); part 2 sees it in #General
# with the Bot badge, and the badge on the member list. Host 2 asks to join the
# same bot (`pollis-bot add-host --bot <bot id>`); part 3 types its code, and
# running host 1 verifies the approval and hands over the key. Part 4 revokes
# host 2 and deletes the bot; the script then stops both hosts and removes
# their installations (`pollis-bot forget`).
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

# Each host is its own data directory (and keystore namespace): nothing is shared.
WEBHOOK="127.0.0.1:$((20000 + RANDOM % 20000))"
BOT_PAIR=(pair --name host-1)
BOT_RUN_WEBHOOK=(run --webhook "$WEBHOOK")
BOT_RUN=(run)

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

# The eight-character pairing code (enrollment-SAS alphabet) on the host's
# "Pairing code:" line (stdout).
code_of() {
  grep -E 'Pairing code:' "$WORK/$1.out" | grep -oE '\b[0-9A-HJKMNP-TV-Z]{8}\b' | head -1
}

# Wait for a `pair`/`add-host` process to exit; it prints `paired: bot <id> …` (stderr) on success.
await_paired() {
  local name="$1" secs="$2" pid
  pid="$(eval echo "\${${name}_PID}")"
  for _ in $(seq 1 "$secs"); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 1
  done
  # Runs in a command substitution, so it cannot `wait` on the host: success is the `paired:` line.
  if kill -0 "$pid" 2>/dev/null || ! grep -qE 'paired: bot [0-9A-Z]+' "$WORK/$name.err"; then
    echo "$name did not finish pairing" >&2
    cat "$WORK/$name.out" "$WORK/$name.err" >&2
    exit 1
  fi
  grep -oE 'paired: bot [0-9A-Z]+' "$WORK/$name.err" | awk '{print $3}' | head -1
}

# Wait (up to $3 seconds) until `pollis-bot status` for data dir $1 shows installation $2 connected.
await_connected() {
  local dir="$1" id="$2" secs="$3"
  for _ in $(seq 1 "$secs"); do
    "$BOT_BIN" --data-dir "$dir" status 2>/dev/null | grep -A1 "^$id " | grep -q 'connected' && return 0
    sleep 1
  done
  echo "installation $id in $dir never reported connected" >&2
  "$BOT_BIN" --data-dir "$dir" status >&2 || true
  exit 1
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
BOT_ID="$(await_paired HOST1 180)"
[ -n "$BOT_ID" ] || { echo "host 1 printed no bot id" >&2; cat "$WORK/HOST1.err" >&2; exit 1; }
echo "    bot: $BOT_ID"

echo "==> host 1 runs with the webhook on $WEBHOOK"
"$BOT_BIN" --data-dir "$WORK/host1" "${BOT_RUN_WEBHOOK[@]}" >"$WORK/HOST1.out" 2>"$WORK/HOST1.err" &
HOST1_PID=$!
await_connected "$WORK/host1" "$BOT_ID" 120

echo "==> the bot posts an alert through its webhook"
"$BOT_BIN" --data-dir "$WORK/host1" post --installation "$BOT_ID" --channel General "$ALERT_TEXT"

echo "==> 2/4 the alert carries the Bot badge"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/2-alert.yaml"

echo "==> host 2 asks to join the same bot"
"$BOT_BIN" --data-dir "$WORK/host2" add-host --bot "$BOT_ID" --name host-2 >"$WORK/HOST2.out" 2>"$WORK/HOST2.err" &
HOST2_PID=$!
await_host HOST2 'code' 60
HOST_CODE="$(code_of HOST2)"
[ -n "$HOST_CODE" ] || { echo "no pairing code from host 2" >&2; cat "$WORK/HOST2.out" >&2; exit 1; }
echo "    code: $HOST_CODE"

echo "==> 3/4 admin adds host 2"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e HOST_CODE="$HOST_CODE" "$FLOWS/3-add-host.yaml"
HOST2_BOT="$(await_paired HOST2 180)"
[ "$HOST2_BOT" = "$BOT_ID" ] || { echo "host 2 joined '$HOST2_BOT', not $BOT_ID" >&2; exit 1; }
"$BOT_BIN" --data-dir "$WORK/host2" "${BOT_RUN[@]}" >"$WORK/HOST2.out" 2>"$WORK/HOST2.err" &
HOST2_PID=$!
await_connected "$WORK/host2" "$BOT_ID" 120

echo "==> 4/4 admin revokes host 2 and deletes the bot"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/4-revoke-delete.yaml"

echo "==> both hosts stop and forget the deleted bot"
kill "$HOST1_PID" "$HOST2_PID" 2>/dev/null || true
wait "$HOST1_PID" "$HOST2_PID" 2>/dev/null || true
HOST1_PID=""
HOST2_PID=""
for host in host1 host2; do
  "$BOT_BIN" --data-dir "$WORK/$host" forget --installation "$BOT_ID"
  if [ -n "$(ls -A "$WORK/$host/installations" 2>/dev/null)" ]; then
    echo "$host still holds an installation after forget" >&2
    exit 1
  fi
done

echo "==> bots passed"
