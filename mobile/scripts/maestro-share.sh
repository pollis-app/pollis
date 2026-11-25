#!/usr/bin/env bash
# Share into Pollis from another app (#2100), on the Android emulator.
#
#   mobile/scripts/maestro-share.sh android
#
# Maestro cannot open another app's share sheet, so this script plays the other app: it sends
# ACTION_SEND / ACTION_SEND_MULTIPLE with adb (`am start`), exactly what a share sheet hands
# Pollis's ShareIntakeActivity, and runs .maestro/scripted/share/ between them:
#
#   1-setup          a fresh account, a group with #General
#   (adb)            share a link, Pollis open and unlocked
#   2-text           "Send to" with the link pre-filled -> the Vault and #General -> saved once
#   (adb)            stop Pollis; share a photo
#   3-image-locked   the PIN first (a wrong one shows the app's own countdown), then "Send to"
#                    with the photo -> #General opens, the photo sent
#   (adb)            share two photos (SEND_MULTIPLE)
#   4-multiple       a typed message, the Vault alone -> the Vault opens, both photos saved
#   5-sign-out       sign out
#   (adb)            Pollis is no longer a share target: no activity answers SEND, and the
#                    system share sheet (screenshot) does not list it
#
# Before signing out it also screenshots the system share sheet with Pollis in it: one icon,
# no person, group or channel (there are no Sharing Shortcuts to show).
#
# iOS shares are not routed through the app: the share extension unlocks and sends in place.
# Its run is mobile/plugins/share-extension/harness/run.sh plus the simulator walkthrough in
# mobile/CLAUDE.md -> "Share extension (#2100)".
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

PLATFORM="${1:-android}"
[ "$PLATFORM" = android ] || { echo "maestro-share.sh drives Android only (see the header)" >&2; exit 2; }
HERE="$(cd "$(dirname "$0")/.." && pwd)"
if ! command -v adb >/dev/null; then
  # shellcheck disable=SC1091
  source "$HERE/android-env.sh" >/dev/null
fi
MAE="$HERE/.maestro"
FLOWS="$MAE/scripted/share"
ENV_FILE="$MAE/.env"
PKG="com.pollis.mobile"
INTAKE="$PKG/com.pollisnative.share.ShareIntakeActivity"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"

OUT="$MAE/artifacts/$(date +%Y-%m-%d)/$PLATFORM"
DEBUG="$OUT/.debug-share"
mkdir -p "$DEBUG"

DEVICE="${ANDROID_SERIAL:-$(adb devices | awk '/^emulator-[0-9]+\tdevice$/{print $1; exit}')}"
[ -n "$DEVICE" ] || { echo "no running emulator (boot pollis_e2e first)" >&2; exit 1; }
A() { adb -s "$DEVICE" "$@"; }

part() {
  echo "--> share $1"
  maestro --device "$DEVICE" test --debug-output "$DEBUG" ${ENV_ARGS[@]+"${ENV_ARGS[@]}"} \
    -e MAESTRO_APP_ID="$PKG" "$FLOWS/$1.yaml"
}

shot() {
  A exec-out screencap -p > "$OUT/$1.png"
  echo "    screenshot $OUT/$1.png"
}

# A photo in the shared MediaStore, as a gallery app would share it: its content:// URI.
media_uri() {
  local name="$1"
  A push "$MAE/fixtures/media-viewer.png" "/sdcard/Pictures/$name" >/dev/null
  A shell am broadcast -a android.intent.action.MEDIA_SCANNER_SCAN_FILE \
    -d "file:///sdcard/Pictures/$name" >/dev/null
  local id=""
  for _ in $(seq 1 20); do
    id="$(A shell content query --uri content://media/external/images/media \
      --projection _id --where "_display_name=\'$name\'" 2>/dev/null \
      | grep -o '_id=[0-9]*' | tail -1 | cut -d= -f2 || true)"
    [ -n "$id" ] && break
    sleep 0.5
  done
  [ -n "$id" ] || { echo "the photo $name never reached MediaStore" >&2; exit 1; }
  echo "content://media/external/images/media/$id"
}

# Pollis takes shares only while signed in: the activity is enabled by the app.
answers_send() {
  A shell cmd package query-activities --brief -a android.intent.action.SEND -t text/plain \
    | grep -q "com.pollisnative.share.ShareIntakeActivity"
}

part 1-setup
answers_send || { echo "signed in, but Pollis is not a share target" >&2; exit 1; }

# The system share sheet while signed in: the Pollis icon, nothing else of Pollis.
A shell am start -W -a android.intent.action.SEND -t text/plain \
  --es android.intent.extra.TEXT "chooser check" >/dev/null 2>&1 || true
sleep 3
shot share-00-system-sheet-signed-in
A shell input keyevent KEYCODE_BACK
sleep 1

echo "    (another app shares a link)"
A shell am start -W -n "$INTAKE" -a android.intent.action.SEND -t text/plain \
  --es android.intent.extra.SUBJECT "Pollis" \
  --es android.intent.extra.TEXT "https://pollis.com/share-e2e" >/dev/null
part 2-text

echo "    (Pollis stopped; another app shares a photo)"
A shell am force-stop "$PKG"
PHOTO1="$(media_uri share-e2e-1.png)"
A shell am start -W -n "$INTAKE" -a android.intent.action.SEND -t image/png \
  --eu android.intent.extra.STREAM "$PHOTO1" --grant-read-uri-permission >/dev/null
part 3-image-locked

echo "    (another app shares two photos)"
PHOTO2="$(media_uri share-e2e-2.png)"
PHOTO3="$(media_uri share-e2e-3.png)"
A shell am start -W -n "$INTAKE" -a android.intent.action.SEND_MULTIPLE -t "image/*" \
  --eua android.intent.extra.STREAM "$PHOTO2,$PHOTO3" --grant-read-uri-permission >/dev/null
part 4-multiple

part 5-sign-out
if answers_send; then
  echo "signed out, but Pollis still answers SEND" >&2
  exit 1
fi
A shell am start -W -a android.intent.action.SEND -t text/plain \
  --es android.intent.extra.TEXT "chooser check" >/dev/null 2>&1 || true
sleep 3
shot share-11-system-sheet-signed-out
A shell input keyevent KEYCODE_BACK
if A shell am start -W -n "$INTAKE" -a android.intent.action.SEND -t text/plain \
  --es android.intent.extra.TEXT "x" 2>&1 | grep -q "Error"; then
  echo "    signed out: the intake refuses to start"
fi

find "$DEBUG" -name 'share-*.png' -exec cp {} "$OUT/" \; 2>/dev/null || true
echo "==> share into Pollis: all parts passed ($PLATFORM). Screenshots: $OUT"
