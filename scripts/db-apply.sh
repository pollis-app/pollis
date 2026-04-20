#!/usr/bin/env bash
#
# Apply pending migrations to a libSQL/Turso database via the HTTP pipeline API.
#
# Intended to be invoked from CI with TURSO_URL and TURSO_TOKEN in the env.
# Do not run this locally against production. For dev, run your SQL by hand.
#
# Exit codes: 0 = success (including no-op), non-zero = failure.

set -euo pipefail

: "${TURSO_URL:?must be set (libsql://...)}"
: "${TURSO_TOKEN:?must be set}"

HTTP_URL="${TURSO_URL/libsql:\/\//https:\/\/}"
MIGRATIONS_DIR="${MIGRATIONS_DIR:-src-tauri/src/db/migrations}"

post() {
  curl -sS --fail-with-body -X POST "$HTTP_URL/v2/pipeline" \
    -H "Authorization: Bearer $TURSO_TOKEN" \
    -H "Content-Type: application/json" \
    -d "$1"
}

# Ensure the tracking table exists. Idempotent.
post '{"requests":[
  {"type":"execute","stmt":{"sql":"CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, description TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT (datetime('"'"'now'"'"')))"}},
  {"type":"close"}
]}' > /dev/null

# Current applied versions, one per line.
APPLIED=$(post '{"requests":[
  {"type":"execute","stmt":{"sql":"SELECT version FROM schema_migrations ORDER BY version"}},
  {"type":"close"}
]}' | jq -r '.results[0].response.result.rows[]?[0].value')

# Adoption: if schema_migrations is empty but the DB already has user data
# (the `users` table exists), silently record v0 instead of running the baseline.
# This handles first-run against a DB that predates this tooling.
if [ -z "${APPLIED:-}" ]; then
  HAS_USERS=$(post '{"requests":[
    {"type":"execute","stmt":{"sql":"SELECT 1 FROM sqlite_master WHERE type='"'"'table'"'"' AND name='"'"'users'"'"' LIMIT 1"}},
    {"type":"close"}
  ]}' | jq -r '.results[0].response.result.rows | length')
  if [ "$HAS_USERS" -gt 0 ]; then
    echo "Adopting existing DB: recording v0 baseline without running baseline SQL"
    post '{"requests":[
      {"type":"execute","stmt":{"sql":"INSERT INTO schema_migrations (version, description) VALUES (0, '"'"'baseline'"'"')"}},
      {"type":"close"}
    ]}' > /dev/null
    APPLIED="0"
  fi
fi

# Collect pending migration files in version order.
PENDING=()
for f in "$MIGRATIONS_DIR"/*.sql; do
  [ -e "$f" ] || continue
  base=$(basename "$f")
  version=$(echo "$base" | sed -E 's/^0*([0-9]+)_.*/\1/')
  if ! printf '%s\n' ${APPLIED:-} | grep -qx "$version"; then
    PENDING+=("$f")
  fi
done

if [ ${#PENDING[@]} -eq 0 ]; then
  echo "No pending migrations."
  exit 0
fi

for f in "${PENDING[@]}"; do
  base=$(basename "$f")
  version=$(echo "$base" | sed -E 's/^0*([0-9]+)_.*/\1/')
  description=$(echo "$base" | sed -E 's/^[0-9]+_(.*)\.sql/\1/' | tr '_' ' ')
  echo "Applying $base (v$version)..."

  # Build a batch of DDL steps + the tracking-row insert, each conditioned on
  # the previous step succeeding. libsql rolls the whole batch back on any
  # step failure.
  BODY=$(jq -n \
    --rawfile sql "$f" \
    --arg version "$version" \
    --arg description "$description" \
    '
    def stmts:
      $sql
      | gsub("(?m)^\\s*--.*$"; "")
      | split(";")
      | map(gsub("^\\s+|\\s+$"; ""))
      | map(select(length > 0));
    {
      requests: [
        {
          type: "batch",
          batch: {
            steps: (
              (stmts | to_entries | map({
                stmt: {sql: .value},
                condition: (if .key == 0 then null else {type:"ok", step:(.key - 1)} end)
              }))
              + [{
                stmt: {
                  sql: "INSERT INTO schema_migrations (version, description) VALUES (?, ?)",
                  args: [
                    {type:"integer", value:$version},
                    {type:"text",    value:$description}
                  ]
                },
                condition: {type:"ok", step:((stmts | length) - 1)}
              }]
            )
          }
        },
        {type: "close"}
      ]
    }')

  RESP=$(post "$BODY")

  STEP_ERRORS=$(echo "$RESP" | jq -c '.results[0].response.result.step_errors // []')
  FIRST_ERROR=$(echo "$STEP_ERRORS" | jq -r '[.[] | select(. != null)][0] // empty')
  if [ -n "$FIRST_ERROR" ]; then
    echo "Migration $base FAILED:"
    echo "$FIRST_ERROR" | jq .
    echo "(batch was rolled back)"
    exit 1
  fi

  echo "  ✓ v$version applied"
done

echo "Done. ${#PENDING[@]} migration(s) applied."
