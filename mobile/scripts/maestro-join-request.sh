#!/usr/bin/env bash
# Join requests on mobile, two devices (#1216): the ADMIN creates a group and
# opens it from its Groups-tab header; the PEER asks to join by slug; the admin
# sees the request on the Groups tab and approves it; the peer then has the
# group.
#
#   mobile/scripts/maestro-join-request.sh <admin-device-id> <peer-device-id>
#
# Both devices need a Release build against the dev DS, and .maestro/.env.
set -euo pipefail

ADMIN="${1:?usage: maestro-join-request.sh <admin-device-id> <peer-device-id>}"
PEER="${2:?usage: maestro-join-request.sh <admin-device-id> <peer-device-id>}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"
FLOWS="$HERE/.maestro/two-client/join-request"
ENV_FILE="$HERE/.maestro/.env"

ENV_ARGS=()
while IFS= read -r line; do
  case "$line" in ''|\#*|MAESTRO_EMAIL=*) continue;; esac
  ENV_ARGS+=(-e "$line")
done < "$ENV_FILE"
BASE="$(sed -n 's/^MAESTRO_EMAIL=//p' "$ENV_FILE" | head -1)"
BASE="${BASE:-pollis-e2e+primary@example.com}"
STAMP="$(date +%Y%m%d%H%M%S)"
ADMIN_EMAIL="${BASE%@*}-jradmin-$STAMP@${BASE#*@}"
PEER_EMAIL="${BASE%@*}-jrpeer-$STAMP@${BASE#*@}"
GROUP_NAME="Join Test $STAMP"
# derive_slug (pollis-api/src/directory.rs): lowercase, spaces to hyphens.
GROUP_SLUG="join-test-$STAMP"
G=(-e GROUP_NAME="$GROUP_NAME" -e GROUP_SLUG="$GROUP_SLUG")

echo "==> 1/4 admin creates \"$GROUP_NAME\" and opens it from the Groups tab"
maestro --device "$ADMIN" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$ADMIN_EMAIL" "$FLOWS/1-admin-create.yaml"
echo "==> 2/4 peer asks to join $GROUP_SLUG"
maestro --device "$PEER" test "${ENV_ARGS[@]}" "${G[@]}" -e MAESTRO_EMAIL="$PEER_EMAIL" "$FLOWS/2-peer-request.yaml"
echo "==> 3/4 admin approves from the Groups tab"
maestro --device "$ADMIN" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/3-admin-approve.yaml"
echo "==> 4/4 peer has the group"
maestro --device "$PEER" test "${ENV_ARGS[@]}" "${G[@]}" "$FLOWS/4-peer-joined.yaml"
echo "==> join request passed"
