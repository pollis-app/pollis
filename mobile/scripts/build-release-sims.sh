#!/usr/bin/env bash
# Build Release apps from the WORKING TREE and install them on the iOS simulator
# and/or the running Android emulator — the checkpoint step before re-running
# the Maestro tour (LOCAL MAC ONLY).
#
# Usage:
#   mobile/scripts/build-release-sims.sh [ios|android|both]      (default: both)
# Env:
#   IOS_UDID        simulator to build for + install on
#                   (default: the "iPhone 18 Pro" simulator, resolved by name)
#   ANDROID_SERIAL  adb serial (default: the first running emulator-*)
#   LAUNCH=0        install only; don't launch the app afterwards
#
# JS-only changes (the re-skin) need NO Rust rebuild: the Release build re-bundles
# the JS from the working tree and links the pollis-core artifacts that are
# already on disk —
#   iOS:     modules/pollis-native/PollisNativeFramework.xcframework (sim slice)
#   Android: modules/pollis-native/android/android/src/main/jniLibs/arm64-v8a
# Rebuild those with ubrn only if pollis-core (Rust) changed — see mobile/CLAUDE.md
# ("Dev loop — Rust changes" / "Dev loop — iOS"). If you do run
# `ubrn build android ... --targets arm64-v8a`, it rewrites the abiFilters in
# modules/pollis-native/android/build.gradle: `git checkout` that file after.
#
# Native projects: ios/ and android/ are generated (gitignored). This script runs
# `expo prebuild --no-install` only when the directory is MISSING (prebuild cleans
# and regenerates since SDK 57); for an app.json / plugin change, delete the
# directory first and re-run. The app's DS URL comes from mobile/.env
# (EXPO_PUBLIC_POLLIS_DELIVERY_URL must be the DEV DS for Maestro runs).
set -euo pipefail

TARGET="${1:-both}"
case "$TARGET" in ios|android|both) ;; *) echo "usage: build-release-sims.sh [ios|android|both]" >&2; exit 1;; esac

MOBILE="$(cd "$(dirname "$0")/.." && pwd)"
cd "$MOBILE"
APP_ID="com.pollis.mobile"
LAUNCH="${LAUNCH:-1}"
EXPO="$MOBILE/node_modules/.bin/expo"
[ -x "$EXPO" ] || { echo "no $EXPO — run: npx -y pnpm@10.25.0 install --ignore-workspace" >&2; exit 1; }

DS_URL="$(sed -n 's/^EXPO_PUBLIC_POLLIS_DELIVERY_URL=//p' .env 2>/dev/null | head -1)"
echo "==> DS baked into the bundle: ${DS_URL:-<unset in mobile/.env>}"
case "$DS_URL" in *api-dev*) ;; *) echo "WARN: Maestro flows expect the dev DS (api-dev.pollis.com)" >&2;; esac

build_ios() {
  local udid="${IOS_UDID:-}"
  if [ -z "$udid" ]; then
    udid="$(xcrun simctl list devices available | grep -F "    iPhone 18 Pro (" | grep -Eo '[0-9A-F-]{36}' | head -1 || true)"
  fi
  [ -n "$udid" ] || { echo "no simulator: set IOS_UDID (xcrun simctl list devices available)" >&2; exit 1; }
  echo "==> iOS: simulator $udid"
  xcrun simctl boot "$udid" 2>/dev/null || true
  xcrun simctl bootstatus "$udid" -b >/dev/null

  [ -d modules/pollis-native/PollisNativeFramework.xcframework/ios-arm64_x86_64-simulator ] \
    || { echo "xcframework has no simulator slice — run ubrn build ios (without --no-sim), then pod install" >&2; exit 1; }

  if [ ! -d ios ]; then
    echo "==> iOS: ios/ missing — expo prebuild + pod install"
    "$EXPO" prebuild --platform ios --no-install
    (cd ios && pod install)
  fi

  mkdir -p ios/build
  local log="ios/build/xcodebuild-release.log"
  echo "==> iOS: xcodebuild Release (arm64 simulator) — log: $log"
  if ! xcodebuild -workspace ios/Pollis.xcworkspace -scheme Pollis \
      -configuration Release -destination "id=$udid" \
      -derivedDataPath ios/build ARCHS=arm64 ONLY_ACTIVE_ARCH=YES build > "$log" 2>&1; then
    grep -E 'error:|BUILD FAILED' "$log" | head -30 >&2 || true
    echo "iOS build FAILED — full log: $MOBILE/$log" >&2; exit 1
  fi
  local app="ios/build/Build/Products/Release-iphonesimulator/Pollis.app"
  [ -d "$app" ] || { echo "no $app after a successful build?" >&2; exit 1; }
  echo "==> iOS: installing $app"
  xcrun simctl install "$udid" "$app"
  if [ "$LAUNCH" = 1 ]; then xcrun simctl launch "$udid" "$APP_ID" >/dev/null && echo "==> iOS: launched"; fi
}

build_android() {
  # shellcheck disable=SC1091
  source "$MOBILE/android-env.sh" >/dev/null
  local serial="${ANDROID_SERIAL:-$(adb devices | awk '/^emulator-[0-9]+\tdevice$/{print $1; exit}')}"
  [ -n "$serial" ] || { echo "no running emulator — boot one first:
  emulator -avd pollis_e2e -no-snapshot -no-boot-anim -no-window -gpu swiftshader_indirect &" >&2; exit 1; }
  echo "==> Android: device $serial"

  [ -d modules/pollis-native/android/android/src/main/jniLibs/arm64-v8a ] \
    || { echo "no arm64-v8a pollis-core libs — run ubrn build android (see header)" >&2; exit 1; }

  if [ ! -d android ]; then
    echo "==> Android: android/ missing — expo prebuild"
    "$EXPO" prebuild --platform android --no-install
  fi

  echo "==> Android: gradle assembleRelease (arm64-v8a)"
  (cd android && ./gradlew :app:assembleRelease -PreactNativeArchitectures=arm64-v8a -q)
  local apk="android/app/build/outputs/apk/release/app-release.apk"
  [ -f "$apk" ] || { echo "no APK at $apk" >&2; exit 1; }
  echo "==> Android: installing $apk"
  adb -s "$serial" install -r "$apk"
  if [ "$LAUNCH" = 1 ]; then
    adb -s "$serial" shell am start -n "$APP_ID/.MainActivity" >/dev/null && echo "==> Android: launched"
  fi
  if ! git diff --quiet -- modules/pollis-native/android/build.gradle 2>/dev/null; then
    echo "WARN: modules/pollis-native/android/build.gradle is modified (ubrn --targets?) — git checkout it before committing" >&2
  fi
}

case "$TARGET" in
  ios) build_ios ;;
  android) build_android ;;
  both) build_ios; build_android ;;
esac
echo "==> done. Next: mobile/scripts/maestro-run.sh tour <ios|android>, then visual-compare.sh"
