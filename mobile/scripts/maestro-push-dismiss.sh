#!/usr/bin/env bash
# Read notifications leave the tray (#1508), on ONE Android device plus a peer.
#
#   mobile/scripts/maestro-push-dismiss.sh <android-device-id>
#
# The peer is `pollis-tui`'s `unread_peer` example: it signs up, waits for the
# device to open a DM with it, and sends when this script says so. The device
# must actually RECEIVE pushes, so this needs:
#   * an Android emulator/phone with Google Play services (FCM);
#   * a Release build whose DS runs #1508's code and can reach Expo — the dev DS
#     once deployed, or a local DS (set POLLIS_DELIVERY_URL for the peer and
#     build the app with the matching EXPO_PUBLIC_POLLIS_DELIVERY_URL).
# Android only: the tray is read with `dumpsys notification`, which iOS has no
# equivalent of.
#
#   1. the device opens a DM, allows notifications, goes to the background;
#   2. the peer sends twice -> two notifications in the tray;
#   3. the app is opened from the LAUNCHER and the DM from the Direct list ->
#      the tray must be empty (before #1508 only a tapped notification left);
#   4. the app is closed, the peer sends again -> one notification, posted with
#      no app process; cold start, read from the list -> the tray must be empty.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-push-dismiss.sh <android-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/push-dismiss"
APP=com.pollis.mobile

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PRIMARY_EMAIL="${BASE%@*}-dismiss-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-dismisspeer-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

# How many of the app's notifications are in the device's tray. Android
# bundles an app's notifications under a summary it posts itself
# (AUTOGROUP_SUMMARY); that is not a message, so it is not counted.
tray_count() {
  adb -s "$DEVICE" shell dumpsys notification --noredact \
    | grep "NotificationRecord(.*pkg=$APP" | grep -vc GROUP_SUMMARY || true
}

# Wait (up to $2 seconds) until the tray holds exactly $1 of the app's notifications.
await_tray() {
  for _ in $(seq 1 "$2"); do
    [ "$(tray_count)" -eq "$1" ] && return 0
    sleep 1
  done
  echo "tray holds $(tray_count) notification(s), expected $1" >&2
  adb -s "$DEVICE" shell dumpsys notification --noredact | grep -A3 "pkg=$APP" >&2 || true
  exit 1
}

echo "==> building the peer"
(cd "$REPO" && cargo build -q -p pollis-tui --example unread_peer)
PEER_BIN="$REPO/target/debug/examples/unread_peer"
PEER_DIR="$(mktemp -d)"
PEER_OUT="$PEER_DIR/peer.out"
export UNREAD_PEER_SIGNAL_DIR="$PEER_DIR/signals"
mkdir -p "$UNREAD_PEER_SIGNAL_DIR"
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

echo "==> peer signs up ($PEER_EMAIL)"
POLLIS_DATA_DIR="$PEER_DIR/data" "$PEER_BIN" "$PEER_EMAIL" >"$PEER_OUT" 2>"$PEER_DIR/peer.err" &
PEER_PID=$!
await_peer PEER_HANDLE= 60
HANDLE="$(sed -n 's/^PEER_HANDLE=//p' "$PEER_OUT")"
echo "    peer handle: $HANDLE"

echo "==> 1/3 device opens a DM with the peer, then goes to the background"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PRIMARY_EMAIL" \
  -e MAESTRO_PEER_HANDLE="$HANDLE" "$FLOWS/1-open-dm.yaml"
await_peer PEER_READY 300
await_tray 0 10

echo "==> peer sends twice while the app is in the background"
touch "$UNREAD_PEER_SIGNAL_DIR/go-1"
await_peer PEER_SENT_1 120
await_tray 2 90
echo "    two notifications in the tray"

echo "==> 2/3 open the app from the launcher, read the DM from the list"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/2-open-from-list.yaml"
await_tray 0 20
echo "    tray cleared by the read"

echo "==> close the app; the peer sends again"
# `am kill`, not force-stop: a force-stopped app is in the stopped state and
# Android delivers it no push at all, which is not what closing it means.
adb -s "$DEVICE" shell am kill "$APP"
touch "$UNREAD_PEER_SIGNAL_DIR/go-2"
await_peer PEER_SENT_2 120
wait "$PEER_PID" || { cat "$PEER_DIR/peer.err" >&2; exit 1; }
PEER_PID=""
await_tray 1 90
echo "    one notification, posted while the app was closed"

echo "==> 3/3 cold start, read from the list"
maestro --device "$DEVICE" test "${ENV_ARGS[@]}" -e MAESTRO_PEER_HANDLE="$HANDLE" \
  "$FLOWS/3-cold-start.yaml"
await_tray 0 20
echo "==> push-dismiss passed"
