#!/usr/bin/env bash
#
# Suspend, unsuspend, or check a Pollis account (#1213). Operator only.
#
#   scripts/suspend-account.sh <user_id> "<reason>"     suspend
#   scripts/suspend-account.sh --lift <user_id>         lift the suspension
#   scripts/suspend-account.sh --show <user_id>         suspension + reports
#
# A suspended account's devices stop authenticating at the Delivery Service
# (pollis-delivery/src/auth.rs), so it can no longer send, create, invite or
# read through the DS. Devices that already have a cached key keep working for
# up to DEVICE_KEY_CACHE_TTL_SECS (30s). Nothing is deleted: lifting restores
# the account exactly as it was.
#
# Reports carry ids and a reason only (no message content exists server-side),
# so decide on account behaviour and report patterns, then email the account
# holder at the address --show prints.
#
# Targets prod by default; DOPPLER_CONFIG=dev for the dev database.

set -euo pipefail

CONFIG="${DOPPLER_CONFIG:-prd_prod}"
TURSO_URL="$(doppler secrets get TURSO_URL -p pollis -c "$CONFIG" --plain)"
TURSO_TOKEN="$(doppler secrets get TURSO_TOKEN -p pollis -c "$CONFIG" --plain)"
HTTP_URL="${TURSO_URL/libsql:\/\//https:\/\/}"

usage() {
  sed -n '3,8p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
}

# One parameterised statement through the libSQL HTTP pipeline; args are
# bound, never interpolated into the SQL.
sql() {
  local stmt="$1"; shift
  local args="[]"
  for a in "$@"; do
    args="$(jq -c --arg v "$a" '. + [{"type":"text","value":$v}]' <<<"$args")"
  done
  curl -sS --fail-with-body -X POST "$HTTP_URL/v2/pipeline" \
    -H "Authorization: Bearer $TURSO_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$(jq -cn --arg sql "$stmt" --argjson args "$args" \
      '{requests:[{type:"execute",stmt:{sql:$sql,args:$args}},{type:"close"}]}')"
}

rows() {
  jq -r '.results[0].response.result.rows[]? | map(.value // "null") | join("  |  ")'
}

[ $# -ge 1 ] || usage

case "$1" in
  --lift)
    [ $# -eq 2 ] || usage
    sql "DELETE FROM account_suspension WHERE user_id = ?" "$2" >/dev/null
    echo "Lifted suspension for $2 ($CONFIG)."
    ;;
  --show)
    [ $# -eq 2 ] || usage
    echo "Account:"
    sql "SELECT id, username, email, created_at FROM users WHERE id = ?" "$2" | rows
    echo "Suspension:"
    sql "SELECT reason, suspended_at FROM account_suspension WHERE user_id = ?" "$2" | rows
    echo "Reports against (reporter | reason | conversation | message | when):"
    sql "SELECT reporter_id, reason, conversation_id, message_id, created_at FROM user_report WHERE reported_id = ? ORDER BY created_at DESC LIMIT 50" "$2" | rows
    ;;
  -*)
    usage
    ;;
  *)
    [ $# -eq 2 ] || usage
    exists="$(sql "SELECT COUNT(*) FROM users WHERE id = ?" "$1" | jq -r '.results[0].response.result.rows[0][0].value')"
    if [ "$exists" != "1" ]; then
      echo "No account with id $1 in $CONFIG." >&2
      exit 1
    fi
    sql "INSERT INTO account_suspension (user_id, reason) VALUES (?, ?) ON CONFLICT(user_id) DO UPDATE SET reason = excluded.reason" "$1" "$2" >/dev/null
    echo "Suspended $1 ($CONFIG): $2"
    ;;
esac
