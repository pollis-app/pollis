#!/bin/sh
# Build a signed bundle from Turso at startup, generate the static /v1 tree, then
# serve it.
#
# NOTE: this is a STARTUP SNAPSHOT — it does not refresh while running. New MLS
# commits won't appear until the container restarts. On-demand (lazy ~60s) refresh
# is tracked as a separate task; until then, restart the container to re-snapshot.
set -eu

: "${TURSO_DATABASE_URL:?set TURSO_DATABASE_URL (e.g. libsql://prod-...turso.io)}"
: "${TURSO_AUTH_TOKEN:?set TURSO_AUTH_TOKEN (read-only)}"
# Keep this STABLE across restarts — auditors pin the public key it derives.
: "${VLOG_SIGNING_KEY:?set VLOG_SIGNING_KEY (32-byte hex from 'builder keygen')}"

PORT="${PORT:-8787}"
DATA="${DATA_DIR:-/data}"
mkdir -p "$DATA"

echo "[transparency] building signed bundle from ${TURSO_DATABASE_URL}"
builder build --db "$TURSO_DATABASE_URL" --out "$DATA/bundle.json" --timestamp "$(date +%s)000"
serve generate --bundle "$DATA/bundle.json" --out "$DATA/site"

echo "[transparency] serving on :${PORT} (startup snapshot — restart to refresh)"
exec serve serve --dir "$DATA/site" --port "$PORT"
