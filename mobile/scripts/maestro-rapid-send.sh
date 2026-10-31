#!/usr/bin/env bash
# Rapid-send e2e (#1636) on an Android emulator, with the race held open for certain.
#
#   mobile/scripts/maestro-rapid-send.sh [adb-serial]      (default: the first running emulator)
#
# Maestro types and taps slowly, so on a fast network each send can land before
# the next tap, and a flow would pass on the old, broken code. This script makes
# the emulator's network slow (emulator console `network delay`) around the
# bursts, so the first send is still going out when Send is tapped again. The
# burst subflow then asserts message 1 is still sending at that moment, so the
# run FAILS instead of passing whenever the race was not reproduced.
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
# The delay is applied to the emulator's own modem, so it slows every request
# the app makes without cutting the network (a cut makes a send FAIL at once,
# which is the outbox's Retry path, not this race). It is always restored on
# exit. Needs a Release build against the dev DS on the emulator and
# .maestro/.env filled in. iOS has no network control from here: run
# `maestro-run.sh rapid-send ios` (flows/rapid-send.yaml, fresh-group timing).
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

HERE="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"
FLOWS="$HERE/.maestro/scripted/rapid-send"
# Per-packet latency while slowed, in ms. A send makes several round trips, so
# this keeps the first one out for well over the few seconds a Maestro type-and-tap takes.
DELAY_MS="${RAPID_SEND_DELAY_MS:-3000}"

SERIAL="${1:-$(adb devices | awk '/^emulator-[0-9]+\tdevice$/{print $1; exit}')}"
[ -n "$SERIAL" ] || { echo "no running emulator (pass an adb serial)" >&2; exit 1; }
case "$SERIAL" in emulator-*) ;; *) echo "$SERIAL is not an emulator: network delay needs the emulator console" >&2; exit 1;; esac

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
EMAIL="${BASE%@*}-rapid-$(date +%Y%m%d%H%M%S)@${BASE#*@}"

net() {
  # `adb emu` talks to the emulator console; it prints OK or KO.
  local out
  out="$(adb -s "$SERIAL" emu network delay "$1" 2>&1 | tr -d '\r')"
  case "$out" in *KO*|*rror*) echo "network delay $1 failed: $out" >&2; return 1;; esac
  echo "    network delay $1"
}
slow() { net "$DELAY_MS"; }
fast() { net none; }
trap 'fast || true' EXIT

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
