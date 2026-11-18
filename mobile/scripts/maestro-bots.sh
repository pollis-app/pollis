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
# Between parts 3 and 4 the hosts run Apps (#2005, #2018): the driver points
# them at App v1 (`pollis-bot app install`), runs flows/bot-app-approval.yaml in
# the background and, as the admin approves, swaps in v2 (a widening update) and
# then removes the files (the refusal notice); `pollis-bot post` nudges a job so
# the host re-checks and reports. Then a commands App is installed, approved
# (4b-approve-app.yaml) and used from the composer (5-commands.yaml).
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

# ---------------------------------------------------------------- Apps (#2005, #2018)
# The bot's hosts run an App from a directory on this machine. The fixture App
# (pollis-app-host/fixtures/test-app.wasm) is the component; each version is
# that component with a different pollis-app.json, so each has its own App hash.
APP_HOST="api.example.com"
APP_NEW_HOST="hooks.example.com"
APP_COMMAND="echo"
APP_REPLY="echo-$STAMP"
APP_DIR="$WORK/app"
FIXTURE="$REPO/pollis-app-host/fixtures/test-app.wasm"

# Write version $1 of the App into $APP_DIR (component + manifest).
app_version() {
  mkdir -p "$APP_DIR"
  cp "$FIXTURE" "$APP_DIR/app.wasm"
  case "$1" in
    v1) cat >"$APP_DIR/pollis-app.json" <<JSON
{"name":"triage","version":"1.0.0","triggers":["messages"],
 "read":{"channels":["#general"],"history":30},"actions":["post","reply"],"egress":["$APP_HOST"]}
JSON
    ;;
    v2) cat >"$APP_DIR/pollis-app.json" <<JSON
{"name":"triage","version":"2.0.0","triggers":["messages","schedule"],
 "read":{"channels":["#general"],"history":30},"actions":["post","reply"],"egress":["$APP_HOST","$APP_NEW_HOST"],
 "schedules":[{"name":"digest","every":3600}]}
JSON
    ;;
    # An update of 4a's App (whose #General grant 4a narrowed off): "*" is new access, so the
    # admin can grant #General again by switching it on (4b-approve-app.yaml).
    commands) cat >"$APP_DIR/pollis-app.json" <<JSON
{"name":"echo","version":"1.0.0","triggers":["commands","mentions"],
 "read":{"channels":["*"]},"actions":["post","reply"],
 "commands":[{"name":"$APP_COMMAND","description":"Echo for the e2e","arg_hint":"<text>"}]}
JSON
    ;;
  esac
}

# The App hash `app install` prints for host $1.
install_app() {
  "$BOT_BIN" --data-dir "$WORK/$1" app install --installation "$BOT_ID" --artifact "$APP_DIR" \
    | sed -n 's/^ *App hash: *//p' | head -1
}

# Restart both hosts' runtimes (a handler is built at start).
restart_hosts() {
  kill "$HOST1_PID" "$HOST2_PID" 2>/dev/null || true
  wait "$HOST1_PID" "$HOST2_PID" 2>/dev/null || true
  "$BOT_BIN" --data-dir "$WORK/host1" "${BOT_RUN_WEBHOOK[@]}" >"$WORK/HOST1.out" 2>"$WORK/HOST1.err" &
  HOST1_PID=$!
  "$BOT_BIN" --data-dir "$WORK/host2" "${BOT_RUN[@]}" >"$WORK/HOST2.out" 2>"$WORK/HOST2.err" &
  HOST2_PID=$!
  await_connected "$WORK/host1" "$BOT_ID" 120
  await_connected "$WORK/host2" "$BOT_ID" 120
}

# Run one job on host 1 so it re-checks the App and reports (a webhook call is
# a job; an App's webhook takes it raw, so nothing is posted).
nudge() {
  "$BOT_BIN" --data-dir "$WORK/host1" post --installation "$BOT_ID" --channel General "nudge" >/dev/null 2>&1 || true
}

# Wait (up to $2 seconds) until host 1 logs that it runs App hash $1.
await_running() {
  for _ in $(seq 1 "$2"); do
    grep -q "running App $1" "$WORK/HOST1.err" 2>/dev/null && return 0
    kill -0 "$MAESTRO_PID" 2>/dev/null || { echo "the approval flow ended before App $1 ran" >&2; return 1; }
    nudge
    sleep 2
  done
  echo "host 1 never ran App $1" >&2
  cat "$WORK/HOST1.err" >&2
  exit 1
}

echo "==> both hosts are pointed at App v1"
app_version v1
V1_HASH="$(install_app host1)"
install_app host2 >/dev/null
[ -n "$V1_HASH" ] || { echo "app install printed no App hash" >&2; exit 1; }
echo "    v1: $V1_HASH"
restart_hosts
nudge

echo "==> 4a admin approves, narrows and updates the App; the driver swaps versions on the host"
A=(-e APP_HOST="$APP_HOST" -e APP_NEW_HOST="$APP_NEW_HOST")
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" "${A[@]}" -e MAESTRO_EMAIL="$ADMIN_EMAIL" \
  "$HERE/.maestro/flows/bot-app-approval.yaml" &
MAESTRO_PID=$!
await_running "$V1_HASH" 300
# Stage 2 narrows the approved App in place (a new approval of the same hash); only then is there
# an update to install. Swapping sooner puts v2 on the page while stage 2 still edits v1.
await_running "$V1_HASH under approval 2" 300
app_version v2
V2_HASH="$(install_app host1)"
install_app host2 >/dev/null
echo "    v2: $V2_HASH"
nudge
await_running "$V2_HASH" 300
# The refusal stage: the files are gone.
rm -rf "$APP_DIR"
nudge
wait "$MAESTRO_PID"

echo "==> 4b a commands App is approved, then a member picks its command (#2018)"
app_version commands
CMD_HASH="$(install_app host1)"
install_app host2 >/dev/null
restart_hosts
nudge
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$ADMIN_EMAIL" \
  -e APP_COMMAND="$APP_COMMAND" "$FLOWS/4b-approve-app.yaml"
for _ in $(seq 1 60); do grep -q "running App $CMD_HASH" "$WORK/HOST1.err" && break; nudge; sleep 2; done
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "${G[@]}" -e APP_COMMAND="$APP_COMMAND" -e APP_REPLY="$APP_REPLY" \
  "$FLOWS/5-commands.yaml"

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
