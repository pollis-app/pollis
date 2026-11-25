#!/usr/bin/env bash
# Share into Pollis from other apps on the iOS simulator (#2100): the real share sheet, from
# Photos and from Safari, into the PollisShare extension, which unlocks and sends in place.
#
#   mobile/scripts/maestro-share-ios.sh <ios|ipad>
#
#   1-setup          a fresh account, a group with #General (in Pollis)
#   (simctl)         add a photo
#   ios-0-photos     Photos -> Share -> Pollis
#   ios-1-pin        a wrong PIN (the app's own countdown), then the right one -> the picker
#   ios-2-send       the Vault and #General -> sent in place, the sheet closes
#   ios-3-vault      Pollis: the Vault holds it once, saved
#   (simctl)         open a page in Safari
#   ios-0-safari     Safari -> Share -> Pollis; the link is the message
#   ios-1-pin, ios-2-send, ios-3-vault   the link in the Vault
#   ios-0-safari + ios-4-lockout    ten wrong PINs in the extension lock the account out here
#   ios-0-safari + ios-5-signed-out the sheet now says "Sign in to Pollis first"
#
# The account is used up by the lockout; every run signs up a fresh one. Needs a Release build
# with the extension on the simulator, .maestro/.env filled in.
set -euo pipefail

PLATFORM="${1:-ios}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
MAE="$HERE/.maestro"
FLOWS="$MAE/scripted/share"
ENV_FILE="$MAE/.env"
APP="com.pollis.ios"

NAME="${IOS_DEVICE:-iPhone 18 Pro}"
[ "$PLATFORM" = ipad ] && NAME="${IPAD_DEVICE:-iPad Pro 13-inch (M5)}"
DEVICE="$(xcrun simctl list devices available | grep -F "    $NAME (" | grep -Eo '[0-9A-F-]{36}' | head -1 || true)"
[ -n "$DEVICE" ] || { echo "no available simulator named \"$NAME\"" >&2; exit 1; }
xcrun simctl boot "$DEVICE" 2>/dev/null || true
xcrun simctl bootstatus "$DEVICE" -b >/dev/null

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
OUT="$MAE/artifacts/$(date +%Y-%m-%d)/$PLATFORM"
DEBUG="$OUT/.debug-share"
mkdir -p "$DEBUG"
IPAD=0
# Where the share sheet puts the Pollis cell when Safari shares a page (ios-0-safari.yaml).
SHEET_POINT="${SHEET_POINT:-82%,76%}"
if [ "$PLATFORM" = ipad ]; then
  IPAD=1
  SHEET_POINT="${SHEET_POINT_IPAD:-93%,19%}"
fi

part() {
  local flow="$1"
  shift
  echo "--> share $flow"
  maestro --device "$DEVICE" test --debug-output "$DEBUG" ${ENV_ARGS[@]+"${ENV_ARGS[@]}"} \
    -e MAESTRO_APP_ID="$APP" -e IPAD="$IPAD" "$@" "$FLOWS/$flow.yaml"
}

part 1-setup

PHOTO="$(mktemp -d)/share-ios.png"
cp "$MAE/fixtures/media-viewer.png" "$PHOTO"
xcrun simctl addmedia "$DEVICE" "$PHOTO"
part ios-0-photos
part ios-1-pin
part ios-2-send -e TAG=photo
part ios-3-vault -e TAG=photo

xcrun simctl openurl "$DEVICE" "https://pollis.com/learn"
sleep 4
part ios-0-safari -e SHEET_POINT="$SHEET_POINT"
part ios-1-pin
part ios-2-send -e TAG=link
part ios-3-vault -e TAG=link

xcrun simctl openurl "$DEVICE" "https://pollis.com/learn"
sleep 3
part ios-0-safari -e SHEET_POINT="$SHEET_POINT"
part ios-4-lockout

part ios-0-safari -e SHEET_POINT="$SHEET_POINT"
part ios-5-signed-out

find "$DEBUG" -name 'share-*.png' -exec cp {} "$OUT/" \; 2>/dev/null || true
echo "==> share into Pollis (iOS extension): all parts passed ($PLATFORM). Screenshots: $OUT"
