#!/usr/bin/env bash
# Locale smoke (#1725): flows/locale-smoke.yaml once per shipped locale, with the DEVICE language
# set to that locale first, so the app's first-run auto-detect is what selects it. The locale list
# is the catalogue directory listing (frontend/src/i18n/locales), so a new locale needs no edit here.
#
#   mobile/scripts/maestro-locale-smoke.sh <ios|ipad|android> [locale ...]
#
# Screenshots: .maestro/artifacts/<date>/<platform>/locale-<code>-NN-<screen>.png. The device is
# put back to English (en-US) at the end, pass or fail.
set -euo pipefail

PLATFORM="${1:?usage: maestro-locale-smoke.sh <ios|ipad|android> [locale ...]}"
shift
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LOCALES_DIR="$HERE/../frontend/src/i18n/locales"
IOS_DEVICE="${IOS_DEVICE:-iPhone 18 Pro}"
IPAD_DEVICE="${IPAD_DEVICE:-iPad Pro 13-inch (M5)}"

if [ $# -gt 0 ]; then
  LOCALES=("$@")
else
  LOCALES=()
  for d in "$LOCALES_DIR"/*/; do
    LOCALES+=("$(basename "$d")")
  done
fi

# The OS's own spelling: Simplified Chinese is zh-Hans on iOS and zh-CN on Android.
ios_tag() {
  case "$1" in zh) echo "zh-Hans" ;; *) echo "$1" ;; esac
}
android_tag() {
  case "$1" in zh) echo "zh-CN" ;; *) echo "$1" ;; esac
}

ios_udid() {
  local name="$IOS_DEVICE"
  [ "$PLATFORM" = "ipad" ] && name="$IPAD_DEVICE"
  xcrun simctl list devices available | grep -F "    $name (" | grep -Eo '[0-9A-F-]{36}' | head -1
}

set_device_language() {
  local code="$1"
  case "$PLATFORM" in
    ios|ipad)
      local udid tag
      udid="$(ios_udid)"
      tag="$(ios_tag "$code")"
      xcrun simctl boot "$udid" 2>/dev/null || true
      xcrun simctl spawn "$udid" defaults write -g AppleLanguages -array "$tag"
      xcrun simctl spawn "$udid" defaults write -g AppleLocale -string "${tag//-/_}"
      # Global preferences are read at process start; a reboot makes every process see them.
      xcrun simctl shutdown "$udid"
      xcrun simctl boot "$udid"
      xcrun simctl bootstatus "$udid" >/dev/null
      ;;
    android)
      # pollis_e2e is a google_apis image, so adb can be root and set the system locale.
      adb root >/dev/null
      adb wait-for-device
      adb shell setprop persist.sys.locale "$(android_tag "$code")"
      adb shell setprop ctl.restart zygote
      adb wait-for-device
      sleep 5
      adb shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 1; done'
      ;;
  esac
}

restore() {
  echo "==> restoring the device language to English"
  set_device_language "en-US" || true
}
trap restore EXIT

FAILED=()
for code in "${LOCALES[@]}"; do
  echo "==> locale $code"
  set_device_language "$code"
  LOCALE="$code" "$HERE/scripts/maestro-run.sh" locale-smoke "$PLATFORM" || FAILED+=("$code")
done

if [ ${#FAILED[@]} -gt 0 ]; then
  echo "==> locale-smoke FAILED for: ${FAILED[*]}" >&2
  exit 1
fi
echo "==> locale-smoke passed for: ${LOCALES[*]}"
