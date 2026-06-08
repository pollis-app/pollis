#!/bin/sh
# Serve a LIVE, lazily-refreshed view of the transparency log read straight from
# Turso. No build-then-snapshot step: the server holds the signed bundle + the
# /v1 read API in memory and rebuilds it on demand. New MLS commits appear within
# the TTL (default 60s) with no idle DB load — at most one DB pull per TTL,
# however many requests arrive — and a failed refresh keeps serving the last-good
# view rather than crashing.
set -eu

: "${TURSO_DATABASE_URL:?set TURSO_DATABASE_URL (e.g. libsql://prod-...turso.io)}"
: "${TURSO_AUTH_TOKEN:?set TURSO_AUTH_TOKEN (read-only)}"
# Keep this STABLE across restarts — auditors pin the public key it derives.
: "${VLOG_SIGNING_KEY:?set VLOG_SIGNING_KEY (32-byte hex from 'builder keygen')}"

PORT="${PORT:-8787}"
TTL="${VLOG_TTL_SECS:-60}"

echo "[transparency] serving live read API on :${PORT} from ${TURSO_DATABASE_URL} (lazy refresh, ttl ${TTL}s)"
exec serve live --db "$TURSO_DATABASE_URL" --port "$PORT" --ttl-secs "$TTL"
