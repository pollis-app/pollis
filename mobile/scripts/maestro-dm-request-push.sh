#!/usr/bin/env bash
# A DM request pushes the phone once, and a tap opens the request list (#1734), on ONE
# Android device plus dev-DS peers.
#
#   mobile/scripts/maestro-dm-request-push.sh <android-device-id>
#
# The peers are `pollis-tui`'s `dm_request_peer` example: a stranger who signs up, opens
# a DM with the device's account (a message request) and writes into it while the
# request is pending, each step released by this script. The device must actually
# RECEIVE pushes, so this needs:
#   * an Android emulator/phone with Google Play services (FCM);
#   * a Release build of a #1734 binary (it registers the `dm_request` push capability)
#     against a DS running #1734's code — the dev DS once deployed.
# Android only: the tray is read with `dumpsys notification`, which iOS has no
# equivalent of.
#
#   1. the device signs up, gets the notification permission, registers, goes to the
#      background;
#   2. stranger A sends a request -> exactly one notification, "Message request" /
#      "<A> wants to message you";
#   3. A's first message -> still one notification, re-posted under a NEW handle (the
#      request's one message-time push); A's second message -> no push at all: the
#      same single notification, same handle;
#   4. tapping it opens the request list (screen-dm-requests), not the chat; accept;
#   5. stranger B sends a request -> one notification; open the app from the launcher
#      and accept B on the request list -> the tray must be empty.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-dm-request-push.sh <android-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/dm-request-push"
APP=com.pollis.mobile

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-dmreq-$STAMP@${BASE#*@}"
PEER_A_EMAIL="${BASE%@*}-dmreqa-$STAMP@${BASE#*@}"
PEER_B_EMAIL="${BASE%@*}-dmreqb-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

# The app's notification records in the tray, one line each. Android bundles an app's
# notifications under a summary it posts itself (AUTOGROUP_SUMMARY); that is not a
# notification of ours, so it is left out.
tray_records() {
  adb -s "$DEVICE" shell dumpsys notification --noredact \
    | grep "NotificationRecord(.*pkg=$APP" | grep -v GROUP_SUMMARY || true
}
tray_count() { tray_records | grep -c . || true; }
# The tag (= the push handle) of the one notification in the tray.
tray_tag() { tray_records | sed -n 's/.* tag=\([^ ]*\) .*/\1/p' | head -1; }
dump_tray() { adb -s "$DEVICE" shell dumpsys notification --noredact | grep -A40 "pkg=$APP" >&2 || true; }

# Wait (up to $2 seconds) until the tray holds exactly $1 of the app's notifications.
await_tray() {
  for _ in $(seq 1 "$2"); do
    [ "$(tray_count)" -eq "$1" ] && return 0
    sleep 1
  done
  echo "tray holds $(tray_count) notification(s), expected $1" >&2
  dump_tray
  exit 1
}

# The one notification in the tray reads "Message request" / "<$1> wants to message you".
assert_request_text() {
  local dump title text
  dump="$(adb -s "$DEVICE" shell dumpsys notification --noredact | sed -n "/pkg=$APP/,/NotificationRecord(/p")"
  title="$(printf '%s\n' "$dump" | sed -n 's/.*android\.title=String (\(.*\))$/\1/p' | head -1)"
  text="$(printf '%s\n' "$dump" | sed -n 's/.*android\.text=String (\(.*\))$/\1/p' | head -1)"
  echo "    title: $title"
  echo "    text:  $text"
  if [ "$title" != "Message request" ] || [ "$text" != "$1 wants to message you" ]; then
    echo "expected \"Message request\" / \"$1 wants to message you\"" >&2
    dump_tray
    exit 1
  fi
}

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example dm_request_peer)
PEER_BIN="$REPO/target/debug/examples/dm_request_peer"
PEER_DIR="$(mktemp -d)"
mkdir -p "$PEER_DIR/a-signals" "$PEER_DIR/b-signals"
trap 'kill "${PEER_A_PID:-}" "${PEER_B_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

# Wait (up to $3 seconds) until peer $1 printed a line starting with $2.
await_peer() {
  local name="$1" want="$2" pid
  pid="$(cat "$PEER_DIR/$name.pid")"
  for _ in $(seq 1 "$3"); do
    grep -q "^$want" "$PEER_DIR/$name.out" 2>/dev/null && return 0
    if ! kill -0 "$pid" 2>/dev/null; then
      grep -q "^$want" "$PEER_DIR/$name.out" 2>/dev/null && return 0
      cat "$PEER_DIR/$name.err" >&2
      exit 1
    fi
    sleep 1
  done
  echo "peer $name never printed $want" >&2
  cat "$PEER_DIR/$name.err" >&2
  exit 1
}

start_peer() {
  local name="$1" email="$2"
  shift 2
  env DM_REQUEST_PEER_SIGNAL_DIR="$PEER_DIR/$name-signals" POLLIS_DATA_DIR="$PEER_DIR/$name-data" "$@" \
    "$PEER_BIN" "$email" "$PRIMARY_EMAIL" >"$PEER_DIR/$name.out" 2>"$PEER_DIR/$name.err" &
  echo $! >"$PEER_DIR/$name.pid"
}

echo "==> stranger A signs up ($PEER_A_EMAIL)"
start_peer a "$PEER_A_EMAIL"
PEER_A_PID="$(cat "$PEER_DIR/a.pid")"
await_peer a PEER_HANDLE= 90
HANDLE_A="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_DIR/a.out")"
echo "    A: $HANDLE_A"

echo "==> 1/4 device signs up ($PRIMARY_EMAIL)"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  "$FLOWS/1-signup.yaml"
# The app asks for the permission only on the first conversation opened, and this
# account opens none before the request; grant it the way that prompt would.
adb -s "$DEVICE" shell pm grant "$APP" android.permission.POST_NOTIFICATIONS

echo "==> 2/4 device registers its push token, then goes to the background"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$FLOWS/2-register-and-background.yaml"
await_peer a PEER_FOUND_PHONE 120
await_tray 0 10

echo "==> A sends a message request"
touch "$PEER_DIR/a-signals/go-request"
await_peer a PEER_REQUESTED 120
await_tray 1 90
assert_request_text "$HANDLE_A"
TAG_REQUEST="$(tray_tag)"
echo "    one notification (handle ${TAG_REQUEST:0:4}…)"

echo "==> A writes the request's first message: pushed once more, as the same request"
touch "$PEER_DIR/a-signals/go-message-1"
await_peer a PEER_SENT_1 120
TAG_FIRST=""
for _ in $(seq 1 90); do
  TAG_FIRST="$(tray_tag)"
  [ -n "$TAG_FIRST" ] && [ "$TAG_FIRST" != "$TAG_REQUEST" ] && break
  sleep 1
done
if [ -z "$TAG_FIRST" ] || [ "$TAG_FIRST" = "$TAG_REQUEST" ]; then
  echo "the first message posted no new request notification" >&2
  dump_tray
  exit 1
fi
await_tray 1 5
assert_request_text "$HANDLE_A"
echo "    still one notification, re-posted (handle ${TAG_FIRST:0:4}…)"

echo "==> A writes a second message: no push while the request is pending"
touch "$PEER_DIR/a-signals/go-message-2"
await_peer a PEER_SENT_2 120
# A push arrives within seconds when it is sent at all (the one above did); give a
# second one three times as long to show up before calling it absent.
sleep 45
await_tray 1 1
if [ "$(tray_tag)" != "$TAG_FIRST" ]; then
  echo "the second message was pushed (handle changed)" >&2
  dump_tray
  exit 1
fi
assert_request_text "$HANDLE_A"
echo "    still the same single notification"
wait "$PEER_A_PID" || { cat "$PEER_DIR/a.err" >&2; exit 1; }
PEER_A_PID=""

echo "==> 3/4 tap the notification: the request list opens; accept"
adb -s "$DEVICE" shell cmd statusbar expand-notifications
sleep 2
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE_A" \
  "$FLOWS/3-tap-opens-requests.yaml"
await_tray 0 20
echo "    request list opened from the tap; tray empty"

echo "==> stranger B ($PEER_B_EMAIL) sends a request while the app is in the background"
touch "$PEER_DIR/b-signals/go-request"
start_peer b "$PEER_B_EMAIL" DM_REQUEST_PEER_MESSAGES=0
PEER_B_PID="$(cat "$PEER_DIR/b.pid")"
await_peer b PEER_HANDLE= 90
HANDLE_B="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_DIR/b.out")"
echo "    B: $HANDLE_B"
await_peer b PEER_REQUESTED 180
await_tray 1 90
assert_request_text "$HANDLE_B"

echo "==> 4/4 open from the launcher and accept B on the request list: the tray empties"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE_B" \
  "$FLOWS/4-accept-clears.yaml"
await_tray 0 30
wait "$PEER_B_PID" || { cat "$PEER_DIR/b.err" >&2; exit 1; }
PEER_B_PID=""
echo "==> dm-request-push passed"
