#!/usr/bin/env bash
# Rapid-send e2e (#1636) on an Android emulator, with the race held open for certain.
#
#   mobile/scripts/maestro-rapid-send.sh [adb-serial]      (default: the first running emulator)
#
# Maestro types and taps slowly, so on a fast network each send can land before
# the next tap, and a flow would pass on the old, broken code. This script makes
# the emulator's network slow around the bursts, so the first send is still
# going out when Send is tapped again. The burst subflow then asserts message 1
# is still sending at that moment, so the run FAILS instead of passing whenever
# the race was not reproduced.
#
#   1-setup                 full speed: sign in, a group, one delivered warm-up
#   [network slowed]
#   2-burst                 four sends back to back while the first is out
#   [network restored]
#   3-delivered             each lands once, in order, no Retry
#   [network slowed]
#   4-burst-kill            a second burst, the app killed mid-delivery, relaunched
#   [network restored]
#   5-delivered-after-kill  each lands once, in order, no Retry
#
# How it slows the network: the emulator must be BOOTED through the local proxy
# scripts/slow-proxy.py on 127.0.0.1:8899 (RAPID_SEND_PROXY_PORT):
#
#   $ANDROID_HOME/emulator/emulator -avd <avd> -no-window -gpu swiftshader_indirect \
#     -http-proxy http://127.0.0.1:8899
#
# This script starts the proxy when nothing listens on that port; boot the
# emulator after it, or run the proxy yourself and pass its switch file as
# RAPID_SEND_SWITCH. While slowed, the proxy holds every chunk back
# RAPID_SEND_DELAY_MS (3000): requests are slow but never cut. A cut (airplane
# mode, Wi-Fi/data off) makes a send FAIL at once, which is the outbox's Retry
# path and not this race. The emulator console's `network delay` does not shape
# traffic on current emulator builds (checked on API 36: a send still landed in
# under a second). The network is always restored on exit.
#
# Needs a Release build against the dev DS on the emulator and .maestro/.env
# filled in. iOS has no network control from here: run
# `maestro-run.sh rapid-send ios` (flows/rapid-send.yaml, fresh-group timing).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

HERE="$(cd "$(dirname "$0")/.." && pwd)"
if ! command -v adb >/dev/null; then
  # shellcheck disable=SC1091
  source "$HERE/android-env.sh" >/dev/null
fi
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/scripted/rapid-send"
# Per-chunk delay while slowed, in ms. A send makes several round trips, so this
# keeps the first one out well past the few seconds a Maestro type-and-tap takes.
DELAY_MS="${RAPID_SEND_DELAY_MS:-3000}"
PROXY_PORT="${RAPID_SEND_PROXY_PORT:-8899}"

SERIAL="${1:-$(adb devices | awk '/^emulator-[0-9]+\tdevice$/{print $1; exit}')}"
[ -n "$SERIAL" ] || { echo "no running emulator (pass an adb serial)" >&2; exit 1; }
case "$SERIAL" in
  emulator-*) ;;
  *) echo "$SERIAL is not an emulator: the slowed network needs one booted with -http-proxy" >&2; exit 1;;
esac

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
EMAIL="${BASE%@*}-rapid-$(date +%Y%m%d%H%M%S)@${BASE#*@}"

PROXY_PID=""
if nc -z 127.0.0.1 "$PROXY_PORT" 2>/dev/null; then
  SWITCH="${RAPID_SEND_SWITCH:?a proxy already listens on $PROXY_PORT: set RAPID_SEND_SWITCH to its switch file}"
else
  SWITCH="$(mktemp -u "${TMPDIR:-/tmp}/rapid-send-slow.XXXXXX")"
  python3 -I "$HERE/scripts/slow-proxy.py" "$PROXY_PORT" "$SWITCH" "$DELAY_MS" &
  PROXY_PID=$!
  sleep 1
fi
slow() { touch "$SWITCH"; echo "    network slowed (${DELAY_MS} ms per chunk)"; }
fast() { rm -f "$SWITCH"; echo "    network at full speed"; }
cleanup() {
  fast || true
  if [ -n "$PROXY_PID" ]; then
    kill "$PROXY_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

run() {
  echo "==> $1"
  maestro --device "$SERIAL" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/$1.yaml"
}

fast
run 1-setup
slow
run 2-burst
fast
run 3-delivered
slow
run 4-burst-kill
fast
run 5-delivered-after-kill
echo "==> rapid-send passed"
