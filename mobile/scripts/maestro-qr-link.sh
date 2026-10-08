#!/usr/bin/env bash
# Two-device QR device link e2e (#1207): an EXISTING device shows a QR from
# Security, a NEW device signs in with that code (no email OTP) — entered as
# text, because a simulator camera cannot be pointed at another screen — the
# existing device approves the tag-verified request, and the new device must
# set its PIN and see the existing device's group.
#
#   mobile/scripts/maestro-qr-link.sh <existing-device-id> <new-device-id>
#
# Prefer an iOS simulator as the NEW device: Maestro types the ~70-character
# code one key at a time, which on an Android emulator can take longer than
# the code's 60-second lifetime (the claim then fails as expired).
#
# Both devices need a Release build against a dev DS that has the link
# endpoints, and .maestro/.env filled in.
set -euo pipefail

EXISTING="${1:?usage: maestro-qr-link.sh <existing-device-id> <new-device-id>}"
NEW="${2:?usage: maestro-qr-link.sh <existing-device-id> <new-device-id>}"
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
EMAIL="${BASE%@*}-qr-$(date +%Y%m%d%H%M%S)@${BASE#*@}"

echo "==> 1/5 existing device signs up ($EMAIL) and creates a group"
maestro --device "$EXISTING" test "${ENV_ARGS[@]}" -e MAESTRO_EMAIL="$EMAIL" "$LINK/1-existing-signup.yaml"

# The new device's reset + navigation runs BEFORE the code exists: a code
# lives 60 s, and clearState + launch + navigation + typing it overran that.
echo "==> 2/5 new device opens manual code entry; existing device shows a link code"
maestro --device "$NEW" test "$QR/3a-new-open-manual.yaml"
maestro --device "$EXISTING" test "$QR/2-existing-show-code.yaml"

# Read the payload off the existing device's screen (the Code tab text).
# `uiautomator dump` waits for an idle screen, which the code screen never is
# (its expiry countdown re-renders every second), so on Android it can fail
# with "could not get idle state"; Maestro's hierarchy reader does not wait.
# The code is rendered with a zero-width space between characters (so it can
# wrap anywhere); drop them, raw or JSON-escaped, before matching.
strip_zwsp() { sed -e $'s/\xe2\x80\x8b//g' -e 's/\\u200[bB]//g'; }
TREE=""
if [[ "$EXISTING" == emulator-* || "$EXISTING" == *:* ]]; then
  if adb -s "$EXISTING" shell uiautomator dump /sdcard/qr-ui.xml >/dev/null 2>&1; then
    TREE="$(adb -s "$EXISTING" exec-out cat /sdcard/qr-ui.xml | strip_zwsp)"
  fi
fi
if ! printf '%s' "$TREE" | grep -q 'pollis-link:v1:'; then
  TREE="$(maestro --device "$EXISTING" hierarchy 2>/dev/null | strip_zwsp)"
fi
PAYLOAD="$(printf '%s' "$TREE" | grep -oE 'pollis-link:v1:[0-9A-Za-z]+:[A-Za-z0-9_-]{43}' | head -1)"
[ -n "$PAYLOAD" ] || { echo "could not read the link code from the existing device" >&2; exit 1; }
echo "    code: ${PAYLOAD:0:28}…"

echo "==> 3/5 new device signs in with the code"
maestro --device "$NEW" test -e PAYLOAD="$PAYLOAD" "$QR/3-new-paste-code.yaml"

echo "==> 4/5 existing device approves the tag-verified request"
maestro --device "$EXISTING" test "$QR/4-existing-approve.yaml"

echo "==> 5/5 new device sets its PIN, finalizes, and sees the group"
maestro --device "$NEW" test "$QR/5-new-finish.yaml"
echo "==> QR link passed"
