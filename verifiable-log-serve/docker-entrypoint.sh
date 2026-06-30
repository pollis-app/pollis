#!/bin/sh
# Serve a LIVE, lazily-refreshed view of the transparency log read straight from
# Turso. No build-then-snapshot step: the server holds the signed bundle + the
# /v1 read API in memory and rebuilds it on demand. New MLS commits appear within
# the TTL (default 60s) with no idle DB load — at most one DB pull per TTL,
# however many requests arrive — and a failed refresh keeps serving the last-good
# view rather than crashing.
set -eu

# The main DB (account_key_log etc.). Still required for the URL/token fallback.
: "${TURSO_DATABASE_URL:?set TURSO_DATABASE_URL (e.g. libsql://prod-...turso.io)}"
: "${TURSO_AUTH_TOKEN:?set TURSO_AUTH_TOKEN (read-only)}"
# Keep this STABLE across restarts — auditors pin the public key it derives.
: "${VLOG_SIGNING_KEY:?set VLOG_SIGNING_KEY (32-byte hex from 'builder keygen')}"

# The live server reads ONLY mls_commit_log, which Goal A moves into its own log
# DB with its own credentials. Default both to the main DB so a single-DB /
# pre-cutover deployment behaves exactly as before. LOG_DB_AUTH_TOKEN must be
# exported so the binary's connect_with_token fallback picks it up.
LOG_DB_URL="${LOG_DB_URL:-$TURSO_DATABASE_URL}"
LOG_DB_AUTH_TOKEN="${LOG_DB_AUTH_TOKEN:-$TURSO_AUTH_TOKEN}"
export LOG_DB_AUTH_TOKEN

PORT="${PORT:-8787}"
TTL="${VLOG_TTL_SECS:-60}"

echo "[transparency] serving live read API on :${PORT} from ${LOG_DB_URL} (lazy refresh, ttl ${TTL}s)"
exec serve live --log-db "$LOG_DB_URL" --port "$PORT" --ttl-secs "$TTL"
