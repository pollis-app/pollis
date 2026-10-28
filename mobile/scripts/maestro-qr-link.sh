#!/usr/bin/env bash
# Two-phone QR sign-in e2e (#1361): a NEW phone shows a code under "Sign in
# with another device", a SIGNED-IN phone scans it from Security → Scan to sign
# in a device and confirms with its PIN, and the new phone must set its PIN and
# see the signed-in phone's group — no email code, nothing typed on the new one.
#
#   mobile/scripts/maestro-qr-link.sh <signed-in-device-id> <new-device-id>
#
# The scan is entered as text ("Enter a code instead"), because a simulator
# camera cannot be pointed at another screen; it feeds the same handler the
# camera does. The desktop halves of both directions run against a dev-DS peer
# in `maestro-qr-scan.sh`.
#
# Both devices need a Release build against a dev DS that has the link
# endpoints, and .maestro/.env filled in.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

EXISTING="${1:?usage: maestro-qr-link.sh <signed-in-device-id> <new-device-id>}"
NEW="${2:?usage: maestro-qr-link.sh <signed-in-device-id> <new-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
LINK="$HERE/.maestro/two-client/device-link"
QR="$HERE/.maestro/two-client/qr-link"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
EMAIL="${BASE%@*}-qr-$(date +%Y%m%d%H%M%S)-$$@${BASE#*@}"

echo "==> 1/4 signed-in phone signs up ($EMAIL) and creates a group"
maestro --device "$EXISTING" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$LINK/1-existing-signup.yaml"

echo "==> 2/4 new phone shows its sign-in code"
maestro --device "$NEW" test "$QR/2-new-show-code.yaml"

# Read the code off the new phone. The text has a zero-width space between
# characters (so it wraps anywhere); drop them, raw or JSON-escaped.
strip_zwsp() { sed -e $'s/\xe2\x80\x8b//g' -e 's/\\u200[bB]//g'; }
TREE=""
if [[ "$NEW" == emulator-* || "$NEW" == *:* ]]; then
  if adb -s "$NEW" shell uiautomator dump /sdcard/qr-ui.xml >/dev/null 2>&1; then
    TREE="$(adb -s "$NEW" exec-out cat /sdcard/qr-ui.xml | strip_zwsp)"
  fi
fi
if ! printf '%s' "$TREE" | grep -q 'pollis-link:v2:'; then
  TREE="$(maestro --device "$NEW" hierarchy 2>/dev/null | strip_zwsp)"
fi
PAYLOAD="$(printf '%s' "$TREE" | grep -oE 'pollis-link:v2:[0-9A-Za-z]+:[A-Za-z0-9_-]{43}' | head -1)"
[ -n "$PAYLOAD" ] || { echo "could not read the sign-in code from the new phone" >&2; exit 1; }
echo "    code: ${PAYLOAD:0:28}…"

echo "==> 3/4 signed-in phone scans it and confirms with its PIN"
maestro --device "$EXISTING" test -e PAYLOAD="$PAYLOAD" "$QR/3-existing-scan.yaml"

echo "==> 4/4 new phone sets its PIN, finalizes, and sees the group"
maestro --device "$NEW" test "$QR/4-new-finish.yaml"
echo "==> QR sign-in passed"
