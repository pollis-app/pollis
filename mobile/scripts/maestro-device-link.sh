#!/usr/bin/env bash
# Two-device link e2e: an EXISTING device approves a NEW device's sign-in by
# its 8-character code, and the new device must land in the app with the
# existing device's group. Guards the bug where mobile finalized enrollment
# before `set_pin` opened the local DB ("not signed in for DS request
# signing") — every device-linking sign-in failed and nothing caught it,
# because no flow ever ran both halves.
#
#   mobile/scripts/maestro-device-link.sh <existing-device-id> <new-device-id>
#   e.g. mobile/scripts/maestro-device-link.sh <ios-sim-udid> emulator-5554
#
# Both devices need a Release build against the dev DS (see .maestro/README.md)
# and .maestro/.env filled in. Device ids: `xcrun simctl list devices` / `adb devices`.
set -euo pipefail

EXISTING="${1:?usage: maestro-device-link.sh <existing-device-id> <new-device-id>}"
NEW="${2:?usage: maestro-device-link.sh <existing-device-id> <new-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
FLOWS="$HERE/.maestro/two-client/device-link"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
EMAIL="${BASE%@*}-link-$(date +%Y%m%d%H%M%S)@${BASE#*@}"

echo "==> 1/4 existing device signs up ($EMAIL) and creates a group"
maestro --device "$EXISTING" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/1-existing-signup.yaml"

echo "==> 2/4 new device signs in and requests a link"
maestro --device "$NEW" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/2-new-request.yaml"

# Read the code off the new device. Maestro cannot carry a value from one
# device's screen into another device's flow, so take it from the UI tree.
if [[ "$NEW" == emulator-* || "$NEW" == *:* ]]; then
  adb -s "$NEW" shell uiautomator dump /sdcard/link-ui.xml >/dev/null
  TREE="$(adb -s "$NEW" exec-out cat /sdcard/link-ui.xml)"
else
  TREE="$(maestro --device "$NEW" hierarchy 2>/dev/null)"
fi
# Only a WHOLE text value of exactly eight code characters (uiautomator XML
# `text="…"`, Maestro JSON `"text" : "…"`, or — on iOS 27 / RN 0.86, where a
# Text's string lands in the accessibility label — `"accessibilityText" : "…"`),
# never a fragment of a longer string.
CODE="$(printf '%s' "$TREE" | grep -oE '(text|accessibilityText)"? ?[=:] ?"[0-9A-HJKMNP-TV-Z]{8}"' | grep -oE '"[0-9A-HJKMNP-TV-Z]{8}"' | tr -d '"' | head -1)"
[ -n "$CODE" ] || { echo "could not read the verification code from the new device" >&2; exit 1; }
echo "    code: $CODE"

echo "==> 3/4 existing device approves with the code"
maestro --device "$EXISTING" test -e CODE="$CODE" "$FLOWS/3-existing-approve.yaml"

echo "==> 4/4 new device creates its PIN, finalizes, and sees the group"
maestro --device "$NEW" test "$FLOWS/4-new-finish.yaml"
echo "==> device link passed"
