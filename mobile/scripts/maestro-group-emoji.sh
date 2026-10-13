#!/usr/bin/env bash
# Custom emoji permissions on mobile, two devices (#1280): any member may ADD an
# emoji, only an ADMIN may remove one. The peer joins the admin's group through
# the join-request flows (so it is a plain member), adds an emoji and gets no
# remove control for it; the admin then sees the remove control and removes it.
#
#   mobile/scripts/maestro-group-emoji.sh <admin-device-id> <peer-device-id>
#
# Both devices need a Release build against the dev DS, and .maestro/.env.
set -euo pipefail

ADMIN="${1:?usage: maestro-group-emoji.sh <admin-device-id> <peer-device-id>}"
PEER="${2:?usage: maestro-group-emoji.sh <admin-device-id> <peer-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
JOIN="$HERE/.maestro/two-client/join-request"
FLOWS="$HERE/.maestro/two-client/group-emoji"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"

BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
ADMIN_EMAIL="${BASE%@*}-emadmin-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-empeer-$STAMP@${BASE#*@}"
GROUP_NAME="Emoji Test $STAMP"
# derive_slug (pollis-api/src/directory.rs): lowercase, spaces to hyphens.
GROUP_SLUG="emoji-test-$STAMP"
G=(-e GROUP_NAME="$GROUP_NAME" -e GROUP_SLUG="$GROUP_SLUG")

echo "==> 1/6 admin creates \"$GROUP_NAME\""
maestro --device "$ADMIN" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$ADMIN_EMAIL" "$JOIN/1-admin-create.yaml"
echo "==> 2/6 peer asks to join $GROUP_SLUG"
maestro --device "$PEER" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$PEER_EMAIL" "$JOIN/2-peer-request.yaml"
echo "==> 3/6 admin approves"
maestro --device "$ADMIN" test "${ENV_ARGS[@]}" "${G[@]}" "$JOIN/3-admin-approve.yaml"
echo "==> 4/6 peer has the group (as a plain member)"
maestro --device "$PEER" test "${ENV_ARGS[@]}" "${G[@]}" "$JOIN/4-peer-joined.yaml"
echo "==> 5/6 peer adds an emoji and gets no remove control"
maestro --device "$PEER" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/5-peer-adds.yaml"
echo "==> 6/6 admin sees the remove control and removes it"
maestro --device "$ADMIN" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/6-admin-removes.yaml"
echo "==> group emoji permissions passed"
