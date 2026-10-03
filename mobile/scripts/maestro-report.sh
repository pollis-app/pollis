#!/usr/bin/env bash
# Report abuse e2e (#1213) on two devices: the PEER device signs up a fresh
# account (so the reported account always exists on the dev DS), the script
# reads its @handle off the screen, and the REPORTER device runs
# flows/report.yaml against that handle.
#
#   mobile/scripts/maestro-report.sh <reporter-device-id> <peer-device-id>
#
# Both devices need a Release build against a dev DS that has /v1/reports, and
# .maestro/.env filled in.
set -euo pipefail

REPORTER="${1:?usage: maestro-report.sh <reporter-device-id> <peer-device-id>}"
PEER="${2:?usage: maestro-report.sh <reporter-device-id> <peer-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*|MAESTRO_PEER_HANDLE=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
PEER_EMAIL="${BASE%@*}-peer-$STAMP@${BASE#*@}"
REPORTER_EMAIL="${BASE%@*}-rep-$STAMP@${BASE#*@}"

echo "==> 1/2 peer signs up ($PEER_EMAIL)"
maestro --device "$PEER" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$PEER_EMAIL" "$HERE/.maestro/two-client/report/1-peer-signup.yaml"

if [[ "$PEER" == emulator-* || "$PEER" == *:* ]]; then
  adb -s "$PEER" shell uiautomator dump /sdcard/report-ui.xml >/dev/null
  TREE="$(adb -s "$PEER" exec-out cat /sdcard/report-ui.xml)"
else
  TREE="$(maestro --device "$PEER" hierarchy 2>/dev/null)"
fi
HANDLE="$(printf '%s' "$TREE" | grep -oE '@[a-z0-9][a-z0-9_.-]{2,}' | head -1 | tr -d '@')"
[ -n "$HANDLE" ] || { echo "could not read the peer's handle" >&2; exit 1; }
echo "    peer handle: $HANDLE"

echo "==> 2/2 reporter reports the peer"
maestro --device "$REPORTER" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$REPORTER_EMAIL" -e MAESTRO_PEER_HANDLE="$HANDLE" "$HERE/.maestro/flows/report.yaml"
echo "==> report passed"
