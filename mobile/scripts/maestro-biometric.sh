#!/usr/bin/env bash
# Biometric unlock e2e (#1685) on the iOS simulator, the iPad simulator or the Android emulator.
#
#   mobile/scripts/maestro-biometric.sh <ios|ipad|android>
#
# Maestro has no biometric command, so this script enrolls the device's biometrics, runs
# .maestro/scripted/biometric/ in six parts, and answers each system prompt between them:
#
#   1-offer        sign up, cold start, PIN unlock -> the one-time offer -> Turn on   [match]
#   2-on           Security shows it on; cold start shows the loader, no keypad       [no match]
#   3-fallback     Cancel -> the keypad, no PIN error; "Use Face ID" again            [match]
#   4-unlocked     linking still checks the PIN; change PIN 1234 -> 5678; cold start  [match]
#   5-switch       off; on asks for the PIN (the old one is refused)                  [match]
#   6-off          on again; off; cold start is the keypad; no second offer
#
# iOS:     enrolment and matches through the simulator's BiometricKit notifications
#          (`notifyutil -p com.apple.BiometricKit_Sim.pearl.match` / `.nomatch`; both target
#          simulators have Face ID).
# Android: a screen-lock PIN (`adb shell locksettings set-pin`), then a fingerprint enrolled
#          through Settings with `adb emu finger touch 1`; a match is that finger, a no-match
#          an unenrolled one. The emulator's fingerprint is Class 3 (BIOMETRIC_STRONG).
#
# Biometrics are UNENROLLED again on exit, whatever happened, so the rest of the suite (whose
# fresh accounts would otherwise be offered biometric unlock after every PIN unlock) is unchanged.
# Needs a Release build against the dev DS on the device and .maestro/.env filled in.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

PLATFORM="${1:?usage: maestro-biometric.sh <ios|ipad|android>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
if ! command -v adb >/dev/null; then
  # shellcheck disable=SC1091
  source "$HERE/android-env.sh" >/dev/null
fi
MAE="$HERE/.maestro"
FLOWS="$MAE/scripted/biometric"
ENV_FILE="$MAE/.env"
# Seconds between a part ending (its prompt up) and the answer.
SETTLE="${BIOMETRIC_SETTLE:-3}"
ANDROID_LOCK_PIN="${BIOMETRIC_ANDROID_LOCK_PIN:-1111}"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
EMAIL="${BASE%@*}-biometric-$PLATFORM-$(date +%Y%m%d%H%M%S)@${BASE#*@}"

OUT="$MAE/artifacts/$(date +%Y-%m-%d)/$PLATFORM"
DEBUG="$OUT/.debug"
mkdir -p "$DEBUG"

case "$PLATFORM" in
  ios|ipad)
    NAME="${IOS_DEVICE:-iPhone 18 Pro}"
    [ "$PLATFORM" = ipad ] && NAME="${IPAD_DEVICE:-iPad Pro 13-inch (M5)}"
    DEVICE="$(xcrun simctl list devices available | grep -F "    $NAME (" | grep -Eo '[0-9A-F-]{36}' | head -1 || true)"
    [ -n "$DEVICE" ] || { echo "no available simulator named \"$NAME\"" >&2; exit 1; }
    xcrun simctl boot "$DEVICE" 2>/dev/null || true
    xcrun simctl bootstatus "$DEVICE" -b >/dev/null
    "$HERE/scripts/maestro-ios-driver-fix.sh" || true
    enrolled() {
      xcrun simctl spawn "$DEVICE" notifyutil -s com.apple.BiometricKit.enrollmentChanged "$1"
      xcrun simctl spawn "$DEVICE" notifyutil -p com.apple.BiometricKit.enrollmentChanged
    }
    match() { xcrun simctl spawn "$DEVICE" notifyutil -p com.apple.BiometricKit_Sim.pearl.match; }
    nomatch() { xcrun simctl spawn "$DEVICE" notifyutil -p com.apple.BiometricKit_Sim.pearl.nomatch; }
    setup() { enrolled 1; }
    teardown() { enrolled 0 || true; }
    ;;
  android)
    DEVICE="${ANDROID_SERIAL:-$(adb devices | awk '/^emulator-[0-9]+\tdevice$/{print $1; exit}')}"
    [ -n "$DEVICE" ] || { echo "no running emulator (boot pollis_e2e first)" >&2; exit 1; }
    match() { adb -s "$DEVICE" emu finger touch 1 >/dev/null; }
    nomatch() { adb -s "$DEVICE" emu finger touch 2 >/dev/null; }
    setup() {
      if ! adb -s "$DEVICE" shell locksettings verify --old "$ANDROID_LOCK_PIN" >/dev/null 2>&1 \
        || adb -s "$DEVICE" shell locksettings get-disabled 2>/dev/null | grep -q true; then
        adb -s "$DEVICE" shell locksettings set-pin "$ANDROID_LOCK_PIN" >/dev/null
      fi
      # Touch the sensor while the Settings enrolment flow asks for it.
      ( for _ in $(seq 1 90); do adb -s "$DEVICE" emu finger touch 1 >/dev/null 2>&1 || true; sleep 1; done ) &
      local toucher=$!
      MAESTRO_APP_ID=com.android.settings command maestro --device "$DEVICE" test \
        -e LOCK_PIN="$ANDROID_LOCK_PIN" "$FLOWS/android-enroll-fingerprint.yaml" || { kill "$toucher" 2>/dev/null; return 1; }
      kill "$toucher" 2>/dev/null || true
      wait "$toucher" 2>/dev/null || true
    }
    # Clearing the screen lock removes every enrolled fingerprint with it.
    teardown() { adb -s "$DEVICE" shell locksettings clear --old "$ANDROID_LOCK_PIN" >/dev/null 2>&1 || true; }
    ;;
  *) echo "unknown platform: $PLATFORM (want ios|ipad|android)" >&2; exit 1;;
esac

trap teardown EXIT
setup

part() {
  echo "--> biometric $1"
  maestro --device "$DEVICE" test --debug-output "$DEBUG" ${ENV_ARGS[@]+"${ENV_ARGS[@]}"} \
    -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/$1.yaml"
}
answer() {
  sleep "$SETTLE"
  echo "    (sensor: $1)"
  "$1"
}

part 1-offer;     answer match
part 2-on;        answer nomatch
part 3-fallback;  answer match
part 4-unlocked;  answer match
part 5-switch;    answer match
part 6-off

find "$DEBUG" -name 'biometric-*.png' -exec cp {} "$OUT/" \; 2>/dev/null || true
echo "==> biometric unlock: all parts passed ($PLATFORM). Screenshots: $OUT"
