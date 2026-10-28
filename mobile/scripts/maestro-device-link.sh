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
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

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
STAMP="$(date +%Y%m%d%H%M%S)"
EMAIL="${BASE%@*}-link-$STAMP@${BASE#*@}"

echo "==> 1/10 existing device signs up ($EMAIL) and creates a group"
maestro --device "$EXISTING" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$FLOWS/1-existing-signup.yaml"

echo "==> 2/10 new device signs in and requests a link"
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

echo "==> 3/10 existing device approves with the code"
maestro --device "$EXISTING" test -e CODE="$CODE" "$FLOWS/3-existing-approve.yaml"

echo "==> 4/10 new device creates its PIN, finalizes, and sees the group"
maestro --device "$NEW" test "$FLOWS/4-new-finish.yaml"
# #1291: one account, two devices — each device's message reaches the other
# live, and one sent while the other is backgrounded lands on resume.
echo "==> 5/10 new device opens #General"
maestro --device "$NEW" test "$FLOWS/5-new-open.yaml"
echo "==> 6/10 existing device sends"
maestro --device "$EXISTING" test -e STAMP="$STAMP" "$FLOWS/6-existing-send-live.yaml"
echo "==> 7/10 new device sees it live, replies, goes to the background"
maestro --device "$NEW" test -e STAMP="$STAMP" "$FLOWS/7-new-sees-live-then-backgrounds.yaml"
# Let the reply's send finish before the app is backgrounded.
sleep 8
maestro --device "$NEW" test "$FLOWS/7b-new-backgrounds.yaml"
# A short trip to the background keeps the LiveKit connection alive, and the
# message would still arrive live; the bug is the device that was really
# away. Cut the new device's network so its wake-up is lost (an Android
# emulator: airplane mode; on an iOS simulator there is no per-device switch,
# so this step only proves the background case there).
offline() {
  if [[ "$NEW" == emulator-* || "$NEW" == *:* ]]; then
    adb -s "$NEW" shell cmd connectivity airplane-mode "$1" >/dev/null
  fi
}
echo "==> 8/10 existing device sees the reply live"
maestro --device "$EXISTING" test -e STAMP="$STAMP" "$FLOWS/8-existing-sees-reply.yaml"
offline enable
trap 'offline disable' EXIT
echo "==> 9/10 existing device sends while the other is offline in the background"
maestro --device "$EXISTING" test -e STAMP="$STAMP" "$FLOWS/9-existing-sends-away.yaml"
# Long enough for the new device's room to notice it lost the server.
sleep 20
offline disable
sleep 10
echo "==> 10/10 new device returns and catches up"
maestro --device "$NEW" test -e STAMP="$STAMP" "$FLOWS/10-new-resume-catches-up.yaml"
echo "==> device link passed"
