#!/usr/bin/env bash
# A read on the account's other device clears this phone, while it runs (#1628), on ONE device
# plus two dev-DS peers.
#
#   mobile/scripts/maestro-cross-device-unread.sh <device-id>
#
# The peers are `pollis-tui`'s `cross_read_peer` example (a test-harness build that never ships):
#   * `owner` signs up an account as its FIRST device and prints the recovery key; part 1 enrolls
#     the device under test into that account with it;
#   * `sender` (another account) opens a DM with the account and writes when told.
# Then, with the app in the FOREGROUND on the Direct tab:
#   2. the phone opens the empty DM once (allowing notifications) and goes back;
#   3. the sender writes twice -> the DM row counts 2;
#   4. the owner device fetches them, which sends delivered receipts that wake the phone with the
#      same `new_message` a message uses -> still 2 (each receipt used to add one);
#   5. the owner device reads the DM (read receipt + cursor push) -> the DS nudge clears the
#      phone's count and tab badge with no navigation and no resume (the prod repro of #1628).
# And with the app in the BACKGROUND (Android only: the tray is read with `dumpsys notification`):
#   6. the sender writes once -> one notification in the tray;
#   7. the owner device reads it -> the tray empties (#1508's silent dismiss push), and on return
#      to the foreground nothing is unread.
#
# Needs: a Release build against the dev DS on the device, .maestro/.env filled in, and
# POLLIS_DEV_ACCESS_KEY for the dev gate (read from Doppler `dev_personal` when unset). The DS
# must carry #1628 (the `read_cursors_changed` nudge). POLLIS_PEER_TARGET overrides the peers'
# cargo target dir.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

DEVICE="${1:?usage: maestro-cross-device-unread.sh <device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/two-client/cross-device-unread"
APP=com.pollis.mobile
ANDROID=1
if [[ "$DEVICE" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]]; then
  ANDROID=0
fi

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_RECOVERY_KEY=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
OWNER_EMAIL="${BASE%@*}-xread-$STAMP@${BASE#*@}"
SENDER_EMAIL="${BASE%@*}-xreadpeer-$STAMP@${BASE#*@}"

if [ -z "${POLLIS_DEV_ACCESS_KEY:-}" ] && command -v doppler >/dev/null; then
  POLLIS_DEV_ACCESS_KEY="$(doppler secrets get POLLIS_DEV_ACCESS_KEY -p pollis -c dev_personal --plain 2>/dev/null | cut -d, -f1 || true)"
fi
export POLLIS_DEV_ACCESS_KEY="${POLLIS_DEV_ACCESS_KEY:-}"
export POLLIS_DELIVERY_URL="${POLLIS_DELIVERY_URL:-https://api-dev.pollis.com}"
export R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-https://unused.invalid}"
export R2_PUBLIC_URL="${R2_PUBLIC_URL:-https://unused.invalid}"
export LIVEKIT_URL="${LIVEKIT_URL:-wss://unused.invalid}"

TARGET="${POLLIS_PEER_TARGET:-$REPO/target}"
echo "==> building the peers"
(cd "$REPO" && CARGO_TARGET_DIR="$TARGET" cargo build -q -p pollis-tui --example cross_read_peer)
PEER_BIN="$TARGET/debug/examples/cross_read_peer"
PEER_DIR="$(mktemp -d)"
OWNER_OUT="$PEER_DIR/owner.out"
SENDER_OUT="$PEER_DIR/sender.out"
export CROSS_READ_SIGNAL_DIR="$PEER_DIR/signals"
mkdir -p "$CROSS_READ_SIGNAL_DIR"
trap 'kill "${OWNER_PID:-}" "${SENDER_PID:-}" 2>/dev/null || true; rm -rf "$PEER_DIR"' EXIT

await_line() {
  local out="$1" pid="$2" err="$3" want="$4" secs="$5"
  for _ in $(seq 1 "$secs"); do
    grep -q "^$want" "$out" 2>/dev/null && return 0
    kill -0 "$pid" 2>/dev/null || { cat "$err" >&2; exit 1; }
    sleep 1
  done
  echo "peer never printed $want" >&2
  cat "$err" >&2
  exit 1
}
owner_says() { await_line "$OWNER_OUT" "$OWNER_PID" "$PEER_DIR/owner.err" "$1" "$2"; }
sender_says() { await_line "$SENDER_OUT" "$SENDER_PID" "$PEER_DIR/sender.err" "$1" "$2"; }
signal() { touch "$CROSS_READ_SIGNAL_DIR/$1"; }

tray_count() {
  adb -s "$DEVICE" shell dumpsys notification --noredact \
    | grep "NotificationRecord(.*pkg=$APP" | grep -vc GROUP_SUMMARY || true
}
await_tray() {
  for _ in $(seq 1 "$2"); do
    [ "$(tray_count)" -eq "$1" ] && return 0
    sleep 1
  done
  echo "tray holds $(tray_count) notification(s), expected $1" >&2
  exit 1
}
run_flow() {
  local flow="$1"
  shift
  maestro --device "$DEVICE" test "${ENV_ARGS[@]}" "$@" "$FLOWS/$flow"
}

echo "==> owner peer signs up as the account's first device"
POLLIS_DATA_DIR="$PEER_DIR/owner" "$PEER_BIN" owner "$OWNER_EMAIL" >"$OWNER_OUT" 2>"$PEER_DIR/owner.err" &
OWNER_PID=$!
owner_says PEER_USER_ID= 90
KEY="$(sed -n 's/^PEER_RECOVERY_KEY=//p' "$OWNER_OUT")"
OWNER_ID="$(sed -n 's/^PEER_USER_ID=//p' "$OWNER_OUT")"

echo "==> 1/6 device joins the account with the recovery key"
run_flow 1-enroll.yaml -e MAESTRO_EMAIL="$OWNER_EMAIL" -e MAESTRO_RECOVERY_KEY="$KEY"
owner_says PEER_SAW_PHONE= 120

echo "==> sender opens a DM with the account; the owner device accepts"
POLLIS_DATA_DIR="$PEER_DIR/sender" "$PEER_BIN" sender "$SENDER_EMAIL" "$OWNER_ID" \
  >"$SENDER_OUT" 2>"$PEER_DIR/sender.err" &
SENDER_PID=$!
sender_says PEER_SENDER_READY 120
HANDLE="$(sed -n 's/^PEER_SENDER_HANDLE=//p' "$SENDER_OUT")"
owner_says PEER_ACCEPTED 300

echo "==> 2/6 device opens the empty DM once, allows notifications, back to Direct"
run_flow 2-open-dm.yaml -e MAESTRO_PEER_HANDLE="$HANDLE"

echo "==> 3/6 sender writes twice while the Direct tab is on screen"
signal go-1
sender_says PEER_SENT_1 120
run_flow 3-live-count.yaml

echo "==> 4/6 the owner device fetches them: delivered receipts wake the phone"
signal ingest-1
owner_says PEER_INGESTED_1 120
sleep 10
run_flow 4-receipts-add-nothing.yaml

echo "==> 5/6 the owner device reads the DM: read receipt, cursor push, DS nudge"
signal read-1
owner_says PEER_READ_1 120
run_flow 5-read-elsewhere.yaml
if [ "$ANDROID" = 1 ]; then
  await_tray 0 30
fi

if [ "$ANDROID" = 1 ]; then
  echo "==> 6/6 app in the background: one message, then the owner device reads it"
  signal go-2
  sender_says PEER_SENT_2 120
  await_tray 1 90
  echo "    one notification in the tray"
  signal read-2
  owner_says PEER_READ_2 120
  await_tray 0 60
  echo "    tray cleared by the read on the other device"
  run_flow 6-resume.yaml
fi
echo "==> cross-device-unread passed"
