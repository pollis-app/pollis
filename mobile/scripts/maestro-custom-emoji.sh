#!/usr/bin/env bash
# Custom emoji across two devices (#1562): a peer sends a static and an
# animated custom emoji, and the recipient sees both as IMAGES, never as the
# `:shortcode:` fallback text. The sender creates a group and adds the emoji,
# the recipient joins through the join-request flows, then the sender posts
# and the recipient checks the timeline.
#
#   mobile/scripts/maestro-custom-emoji.sh <sender-device-id> <recipient-device-id>
#
# Run it both ways round across platforms (iOS → Android and Android → iOS):
# the bug was iOS-only on the receiving side.
# Both devices need a Release build against the dev DS, and .maestro/.env.
set -euo pipefail
# shellcheck source=maestro-app-id.sh
source "$(dirname "$0")/maestro-app-id.sh"

SENDER="${1:?usage: maestro-custom-emoji.sh <sender-device-id> <recipient-device-id>}"
RECIPIENT="${2:?usage: maestro-custom-emoji.sh <sender-device-id> <recipient-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
JOIN="$HERE/.maestro/two-client/join-request"
FLOWS="$HERE/.maestro/two-client/custom-emoji"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"

BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
SENDER_EMAIL="${BASE%@*}-cesend-$STAMP@${BASE#*@}"
RECIPIENT_EMAIL="${BASE%@*}-cerecv-$STAMP@${BASE#*@}"
GROUP_NAME="Custom Emoji $STAMP"
# derive_slug (pollis-api/src/directory.rs): lowercase, spaces to hyphens.
GROUP_SLUG="custom-emoji-$STAMP"
G=(-e GROUP_NAME="$GROUP_NAME" -e GROUP_SLUG="$GROUP_SLUG")

echo "==> 1/6 sender creates \"$GROUP_NAME\" and adds a static + an animated emoji"
maestro --device "$SENDER" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$SENDER_EMAIL" "$FLOWS/1-sender-create.yaml"
echo "==> 2/6 recipient asks to join $GROUP_SLUG"
maestro --device "$RECIPIENT" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$RECIPIENT_EMAIL" "$JOIN/2-peer-request.yaml"
echo "==> 3/6 sender approves"
maestro --device "$SENDER" test "${ENV_ARGS[@]}" "${G[@]}" "$JOIN/3-admin-approve.yaml"
echo "==> 4/6 recipient has the group"
maestro --device "$RECIPIENT" test "${ENV_ARGS[@]}" "${G[@]}" "$JOIN/4-peer-joined.yaml"
echo "==> 5/6 sender posts both emoji in #General"
maestro --device "$SENDER" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/5-sender-sends.yaml"
echo "==> 6/6 recipient sees both as images"
maestro --device "$RECIPIENT" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/6-recipient-sees.yaml"
echo "==> custom emoji render passed"
