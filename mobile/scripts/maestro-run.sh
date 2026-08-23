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
APP_ID="com.pollis.mobile"

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
    case "$line" in MAESTRO_EMAIL=*) continue;; esac
    ENV_ARGS+=(-e "$line")
  done < "$ENV_FILE"
else
  echo "WARN: $ENV_FILE missing — copy env.example and fill it in." >&2
fi

# MAESTRO_EMAIL is deliberately NOT taken from .env as-is. Every flow opens with
# `clearState` and then signs UP, so a fixed address only works on its very
# first use ever: from the second flow onward the account already exists, auth
# takes the returning-device enrollment path, no create-PIN screen appears, and
# the flow dies on `screen-auth-pin`. That is exactly what the first full run of
# this suite hit — 7 of 8 flows failed that way, all for this one reason.
#
# So each flow gets its own brand-new disposable account, derived from the
# configured address by extending its `+` tag. Enrollment needs the recovery
# key, which the harness has no way to hold, so re-signing-up is the only
# self-contained option.
EMAIL_BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" 2>/dev/null | head -1)"
EMAIL_BASE="${EMAIL_BASE:-pollis-e2e+primary@example.com}"
fresh_email() {
  local local_part="${EMAIL_BASE%@*}" domain="${EMAIL_BASE#*@}"
  echo "${local_part}-$(date +%Y%m%d%H%M%S)-$1@${domain}"
}

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

# Collect the flows to run. `all` becomes an explicit LIST rather than handing
# the directory to Maestro, because one invocation shares one `-e` environment
# and every flow needs its own signup address (see fresh_email above). Running
# them separately also attributes a failure to one flow instead of one batch.
FLOW_FILES=()
if [ "$FLOW" = "all" ]; then
  for f in "$MAE"/flows/*.yaml; do
    [ -e "$f" ] && FLOW_FILES+=("$f")
  done
else
  FLOW_FILES=("$TARGET")
fi

echo "==> running ${#FLOW_FILES[@]} flow(s)  (platform=$PLATFORM)"
# Maestro ignores cwd for screenshot output — takeScreenshot always lands
# under its own ~/.maestro/tests/<run>/ debug tree regardless of `cd`.
# --debug-output redirects that whole tree under DEBUG instead, then we
# flatten the actual PNGs (takeScreenshot = the named gallery shots,
# screenshots/step-* = failure captures) into OUT below.
DEBUG="$OUT/.debug"
mkdir -p "$DEBUG"
FAILED=()
for f in "${FLOW_FILES[@]}"; do
  fname="$(basename "$f" .yaml)"
  echo "--> $fname"
  maestro "${DEVICE_SEL[@]}" test --debug-output "$DEBUG" \
    "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$(fresh_email "$fname")" "$f" || {
    FAILED+=("$fname")
    echo "!! $fname reported failures — screenshots (incl. the failing state) are in $OUT" >&2
  }
done

find "$DEBUG" \( -path "*/takeScreenshot/*.png" -o -path "*/screenshots/*.png" \) -exec cp {} "$OUT/" \; 2>/dev/null || true
rm -rf "$DEBUG"

echo "==> screenshots in: $OUT"
ls -1 "$OUT"/*.png 2>/dev/null || echo "(no screenshots captured)"

if [ ${#FAILED[@]} -gt 0 ]; then
  echo "==> FAILED (${#FAILED[@]}/${#FLOW_FILES[@]}): ${FAILED[*]}" >&2
  exit 1
fi
echo "==> all ${#FLOW_FILES[@]} flow(s) passed"
