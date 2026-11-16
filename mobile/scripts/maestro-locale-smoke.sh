#!/usr/bin/env bash
# Locale smoke (#1725): flows/locale-smoke.yaml once per shipped locale, with the DEVICE language
# set to that locale first, so the app's first-run auto-detect is what selects it. The locale list
# is the catalogue directory listing (frontend/src/i18n/locales), so a new locale needs no edit here.
#
#   mobile/scripts/maestro-locale-smoke.sh <ios|ipad|android> [locale ...]
#
# iOS sets the simulator's system language (and reboots it); Android sets the app's own language,
# which is what the app's process locale follows there. Screenshots:
# .maestro/artifacts/<date>/<platform>/locale-<code>-NN-<screen>.png. The device (iOS) or the app
# (Android) is put back to English at the end, pass or fail.
set -euo pipefail

PLATFORM="${1:?usage: maestro-locale-smoke.sh <ios|ipad|android> [locale ...]}"
shift
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LOCALES_DIR="$HERE/../frontend/src/i18n/locales"
IOS_DEVICE="${IOS_DEVICE:-iPhone 18 Pro}"
IPAD_DEVICE="${IPAD_DEVICE:-iPad Pro 13-inch (M5)}"
ANDROID_APP_ID="com.pollis.mobile"

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
      # The emulator's system language cannot be switched from adb on API 36 without a reboot, so
      # use the per-app language (Settings → Apps → Pollis → Language), which the app's process
      # locale and Hermes' Intl follow. `pm clear` first: a stored choice would win, and clearing
      # afterwards would wipe the per-app language (the flow therefore does not clearState here).
      adb shell pm clear "$ANDROID_APP_ID" >/dev/null
      if [ "$code" = "en-US" ]; then
        adb shell cmd locale set-app-locales "$ANDROID_APP_ID"
      else
        adb shell cmd locale set-app-locales "$ANDROID_APP_ID" --locales "$(android_tag "$code")"
      fi
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
