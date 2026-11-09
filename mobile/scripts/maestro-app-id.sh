# Sourced by every mobile/scripts/maestro-*.sh. The app id differs by platform:
# iOS is `com.pollis.ios` (the App Store team's bundle id; `com.pollis.mobile`
# stays held by the retired personal team), Android is `com.pollis.mobile`.
# Every flow declares `appId: ${MAESTRO_APP_ID}`, and Maestro imports any shell
# variable prefixed MAESTRO_, so this wrapper exports the right id for the
# `--device` each invocation targets: a simulator UDID means iOS, anything else
# (an adb serial such as emulator-5554) means Android.
POLLIS_IOS_APP_ID="com.pollis.ios"
POLLIS_ANDROID_APP_ID="com.pollis.mobile"
POLLIS_MAESTRO_SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

maestro() {
  local prev="" arg device=""
  for arg in "$@"; do
    if [ "$prev" = "--device" ]; then
      device="$arg"
    fi
    prev="$arg"
  done
  if [[ "$device" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]]; then
    MAESTRO_APP_ID="$POLLIS_IOS_APP_ID" command maestro "$@"
    return
  fi
  # Android flows that seed photos (#1769): clear earlier runs' copies first,
  # and keep the photo picker synced while the flow runs, or it never lists
  # what `addMedia` adds. See maestro-media.sh.
  local seeds="" rc=0 watcher=""
  for arg in "$@"; do
    if [ -f "$arg" ] && grep -q -- '- addMedia:' "$arg"; then
      seeds=1
    fi
  done
  if [ -n "$seeds" ] && [ -n "$device" ]; then
    "$POLLIS_MAESTRO_SCRIPTS/maestro-media.sh" reset "$device"
    "$POLLIS_MAESTRO_SCRIPTS/maestro-media.sh" watch "$device" &
    watcher=$!
  fi
  MAESTRO_APP_ID="${MAESTRO_APP_ID:-$POLLIS_ANDROID_APP_ID}" command maestro "$@" || rc=$?
  if [ -n "$watcher" ]; then
    kill "$watcher" 2>/dev/null || true
    wait "$watcher" 2>/dev/null || true
  fi
  return "$rc"
}
