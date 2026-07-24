#!/usr/bin/env bash
# Pollis mobile e2e + visual runner (#619). LOCAL MAC ONLY — boots the target
# simulator/emulator, runs a Maestro flow (or the whole suite), and collects the
# screenshots into a dated, per-platform gallery for visual review.
#
# Usage:
#   mobile/scripts/maestro-run.sh <flow|all> <ios|ipad|android>
# Examples:
#   mobile/scripts/maestro-run.sh auth ios
#   mobile/scripts/maestro-run.sh all ipad
#
# Prereqs (see .maestro/README.md): Maestro installed; a DEV build of the app
# installed on the target device (points at api-dev.pollis.com); .maestro/.env
# filled from env.example. Device names are overridable via env vars below.
set -euo pipefail

FLOW="${1:?usage: maestro-run.sh <flow|all> <ios|ipad|android>}"
PLATFORM="${2:?usage: maestro-run.sh <flow|all> <ios|ipad|android>}"

HERE="$(cd "$(dirname "$0")/.." && pwd)"        # mobile/
MAE="$HERE/.maestro"
ENV_FILE="$MAE/.env"
APP_ID="com.anonymous.mobile"

# Device names — override to match your simulators/emulators.
IOS_DEVICE="${IOS_DEVICE:-iPhone 17 Pro}"
IPAD_DEVICE="${IPAD_DEVICE:-iPad Pro 13-inch (M4)}"
ANDROID_AVD="${ANDROID_AVD:-Pixel_8_API_35}"

# Resolve the flow path.
if [ "$FLOW" = "all" ]; then
  TARGET="$MAE/flows"
else
  TARGET="$MAE/flows/${FLOW%.yaml}.yaml"
  [ -f "$TARGET" ] || { echo "no such flow: $TARGET" >&2; exit 1; }
fi

# Load -e env args from .maestro/.env (KEY=VALUE lines).
ENV_ARGS=()
if [ -f "$ENV_FILE" ]; then
  while IFS= read -r line; do
    case "$line" in ''|\#*) continue;; esac
    ENV_ARGS+=(-e "$line")
  done < "$ENV_FILE"
else
  echo "WARN: $ENV_FILE missing — copy env.example and fill it in." >&2
fi

# Boot the device and pick the Maestro --device selector.
DEVICE_SEL=()
case "$PLATFORM" in
  ios|ipad)
    NAME="$IOS_DEVICE"; [ "$PLATFORM" = "ipad" ] && NAME="$IPAD_DEVICE"
    echo "==> booting iOS simulator: $NAME"
    xcrun simctl boot "$NAME" 2>/dev/null || true
    open -a Simulator || true
    UDID="$(xcrun simctl list devices | grep -F "$NAME (" | grep -Eo '[0-9A-F-]{36}' | head -1)"
    [ -n "$UDID" ] && DEVICE_SEL=(--device "$UDID")
    ;;
  android)
    echo "==> booting Android emulator: $ANDROID_AVD"
    ( "$ANDROID_HOME/emulator/emulator" -avd "$ANDROID_AVD" -no-snapshot -no-boot-anim >/dev/null 2>&1 & )
    adb wait-for-device
    # give the launcher a moment
    adb shell 'while [ "$(getprop sys.boot_completed)" != "1" ]; do sleep 1; done'
    ;;
  *) echo "unknown platform: $PLATFORM (want ios|ipad|android)" >&2; exit 1;;
esac

# Output gallery: artifacts/<YYYY-MM-DD>/<platform>/
DATE="$(date +%Y-%m-%d)"
OUT="$MAE/artifacts/$DATE/$PLATFORM"
mkdir -p "$OUT"

echo "==> running: $TARGET  (platform=$PLATFORM)"
# Run from OUT so takeScreenshot writes the PNGs into the gallery dir.
( cd "$OUT" && maestro "${DEVICE_SEL[@]}" test "${ENV_ARGS[@]}" "$TARGET" ) || {
  echo "!! flow reported failures — screenshots (incl. the failing state) are in $OUT" >&2
}

echo "==> screenshots in: $OUT"
ls -1 "$OUT"/*.png 2>/dev/null || echo "(no screenshots captured)"
