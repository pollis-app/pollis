#!/usr/bin/env bash
#
# Backend fixtures for the authenticated desktop e2e flow (M1 of #570).
#
# `e2e/e2e.js` (full signup) and `e2e/invalid-otp.js` need a REAL backend the
# smoke test doesn't: a writable Turso DB with the schema applied, plus the real
# `pollis-delivery` binary issuing/verifying the dev OTP. This script stands all
# of that up on loopback and prints the env those scripts consume, so CI (and a
# local run) no longer depend on a hand-provisioned Turso.
#
# It brings up, in order:
#   1. a real libsql server (Turso `libsql-server` / sqld) on 127.0.0.1:$LIBSQL_PORT,
#      in its default no-auth local mode (so TURSO_TOKEN is an ignored placeholder);
#   2. the remote schema, applied by the repo's real migration runner
#      (scripts/db-apply.sh over the /v2/pipeline HTTP API) — NOT hand-written SQL;
#   3. the REAL `target/debug/pollis-delivery` binary on 127.0.0.1:$DS_PORT with
#      DEV_OTP=000000 and RESEND disabled (matches src-tauri/tests/flows/harness.rs
#      `spawn_in_process_delivery`, but the shipped binary instead of an in-process copy).
#
# Then it emits the env the e2e scripts read — TURSO_URL, TURSO_TOKEN,
# POLLIS_DELIVERY_URL, R2_S3_ENDPOINT, R2_PUBLIC_URL, DEV_OTP — to $GITHUB_ENV
# when running in CI, and as `export ...` lines on stdout for local use
# (`eval "$(e2e/scripts/start-backend.sh)"`). All progress logging goes to
# stderr so that stdout is pure, evaluable `export` lines.
#
# Tear down with e2e/scripts/stop-backend.sh.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUN_DIR="${POLLIS_E2E_RUN_DIR:-$ROOT/e2e/.backend}"
mkdir -p "$RUN_DIR"

# libsql server port; DS port MUST match e2e/lib/harness.js `DS_PORT` (8788) so
# the e2e scripts' app env (POLLIS_DELIVERY_URL) and their orphan-reap set line up.
LIBSQL_PORT="${LIBSQL_PORT:-8080}"
DS_PORT="${DS_PORT:-8788}"
LIBSQL_CONTAINER="${LIBSQL_CONTAINER:-pollis-e2e-libsql}"
# Pinned-by-tag Turso libsql server image (aka sqld). Serves the hrana-over-HTTP
# pipeline the libsql client + scripts/db-apply.sh use, on container port 8080.
LIBSQL_IMAGE="${LIBSQL_IMAGE:-ghcr.io/tursodatabase/libsql-server:latest}"

# The libsql server runs in no-auth mode, so the token is a placeholder it
# ignores — but pollis-delivery/src/main.rs and pollis-core/src/config.rs both
# REQUIRE the var to be present, so it must be non-empty.
TURSO_URL="http://127.0.0.1:${LIBSQL_PORT}"
TURSO_TOKEN="${TURSO_TOKEN:-local-e2e-placeholder}"
POLLIS_DELIVERY_URL="http://127.0.0.1:${DS_PORT}"
# R2 must be PRESENT for Config::from_env() (pollis-core/src/config.rs hard-requires
# R2_S3_ENDPOINT / R2_PUBLIC_URL) or the app panics in its Tauri setup hook — but
# signup never dials R2, so unreachable placeholders are correct. Real object
# storage (MinIO/R2) is deferred to a later milestone (M3), when a media/attachment
# test actually needs it.
R2_S3_ENDPOINT="${R2_S3_ENDPOINT:-http://127.0.0.1:9/r2}"
R2_PUBLIC_URL="${R2_PUBLIC_URL:-http://127.0.0.1:9/r2-public}"

log() { echo "[start-backend] $*" >&2; }

# --- 1. libsql server -------------------------------------------------------
log "starting libsql server ($LIBSQL_IMAGE) on 127.0.0.1:$LIBSQL_PORT"
docker rm -f "$LIBSQL_CONTAINER" >/dev/null 2>&1 || true
docker run -d --name "$LIBSQL_CONTAINER" \
  -p "127.0.0.1:${LIBSQL_PORT}:8080" \
  "$LIBSQL_IMAGE" >/dev/null

# Ready when the pipeline API answers a no-op batch (guaranteed to exist, unlike
# any particular health path). `--fail` so a non-2xx keeps us waiting.
log "waiting for libsql server to accept queries..."
ready=0
for _ in $(seq 1 60); do
  if curl -fsS -X POST "${TURSO_URL}/v2/pipeline" \
       -H "Content-Type: application/json" \
       -d '{"requests":[{"type":"close"}]}' >/dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  log "libsql server did not become ready"
  docker logs "$LIBSQL_CONTAINER" >&2 || true
  exit 1
fi
log "libsql server up"

# --- 2. schema via the real migration runner --------------------------------
# scripts/db-apply.sh reads TURSO_URL/TURSO_TOKEN, rewrites libsql:// -> https://
# (a plain http:// URL is left as-is), and POSTs the migrations in
# pollis-core/src/db/migrations to /v2/pipeline. It's idempotent. Its stdout is
# redirected to stderr so our stdout stays pure `export` lines.
log "applying migrations via scripts/db-apply.sh"
TURSO_URL="$TURSO_URL" TURSO_TOKEN="$TURSO_TOKEN" bash "$ROOT/scripts/db-apply.sh" >&2

# --- 3. real pollis-delivery binary -----------------------------------------
DS_BIN="$ROOT/target/debug/pollis-delivery"
if [ ! -x "$DS_BIN" ]; then
  log "pollis-delivery not built; building (debug)"
  (cd "$ROOT" && cargo build -p pollis-delivery) >&2
fi

# Clear any orphan DS from a prior run holding :$DS_PORT before we launch a fresh
# one (start-backend OWNS the DS lifecycle; the per-run reaps deliberately leave
# it alone). Harmless no-op on an ephemeral CI runner.
pkill -9 -f "target/debug/pollis-delivery" >/dev/null 2>&1 || true

# LiveKit token-broker secrets (issue #570, M3a). The DS's BrokerConfig reads
# LIVEKIT_API_KEY / LIVEKIT_API_SECRET / LIVEKIT_URL and treats EMPTY as unset
# (pollis-delivery/src/broker.rs), so forwarding them unconditionally is safe:
# for the media E2E they're set by e2e/scripts/start-livekit.sh (run before this
# script), and for the M1/M2 flows (no LiveKit) they default empty → the broker
# is simply "not configured" and nothing changes.
LIVEKIT_API_KEY="${LIVEKIT_API_KEY:-}"
LIVEKIT_API_SECRET="${LIVEKIT_API_SECRET:-}"
LIVEKIT_URL="${LIVEKIT_URL:-}"
if [ -n "$LIVEKIT_URL" ]; then
  log "LiveKit configured for the DS (url=$LIVEKIT_URL, key present)"
else
  log "LiveKit not configured (no LIVEKIT_URL) — DS livekit endpoints will 503"
fi

# DEV_OTP=000000 + no RESEND_API_KEY => OTP email is skipped and 000000 is the
# only code that verifies (pollis-delivery/src/otp.rs OtpConfig::from_env).
# LOG_DB_* unset => the MLS control-plane tables share the single libsql DB
# (pollis-delivery/src/main.rs). PORT/TURSO_URL/TURSO_TOKEN are read by main.rs.
# POLLIS_DS_REQUIRE_AUTH=true => ENFORCE device-signature auth (the PRODUCTION
# config; pollis-delivery/src/lib.rs default is OFF). Required for the LiveKit
# path: `ds_livekit_token` is device-signed with NO user_id in the body and
# relies on the DS deriving the user from the verified signature — with auth off
# the broker's resolve_user() 400s "user_id required when auth is disabled"
# (broker.rs), which breaks realtime presence + calls. Signup's bootstrap writes
# are OTP-session-gated (independent of this), so enforcing here is safe.
log "starting pollis-delivery on 127.0.0.1:$DS_PORT (DEV_OTP=000000, auth enforced)"
# FULLY DETACH the DS so it outlives THIS step. In CI (GitHub Actions) a step's
# child processes are reaped with the step's process group when the step's shell
# exits, so a plain `&` DS dies before the later e2e step can reach :8788. setsid
# (new session, escapes the step's process group) + nohup (ignore SIGHUP) +
# </dev/null (detach stdin) + disown keeps it alive until job end / stop-backend.sh.
# Cleanup is by-name (`pkill -f target/debug/pollis-delivery` in stop-backend.sh),
# so a possibly-imprecise $! here is only a best-effort record.
setsid nohup env -u RESEND_API_KEY -u LOG_DB_URL -u LOG_DB_TOKEN -u LOG_DB_ADMIN_TOKEN \
  TURSO_URL="$TURSO_URL" TURSO_TOKEN="$TURSO_TOKEN" \
  LIVEKIT_API_KEY="$LIVEKIT_API_KEY" LIVEKIT_API_SECRET="$LIVEKIT_API_SECRET" LIVEKIT_URL="$LIVEKIT_URL" \
  POLLIS_DS_REQUIRE_AUTH="true" \
  PORT="$DS_PORT" DEV_OTP="000000" RUST_LOG="pollis_delivery=info" \
  "$DS_BIN" > "$RUN_DIR/pollis-delivery.log" 2>&1 < /dev/null &
DS_PID=$!
echo "$DS_PID" > "$RUN_DIR/pollis-delivery.pid"
disown "$DS_PID" 2>/dev/null || true

# /health is an open (no-auth) 200 in pollis-delivery/src/lib.rs.
log "waiting for pollis-delivery /health..."
healthy=0
for _ in $(seq 1 30); do
  if curl -fsS "${POLLIS_DELIVERY_URL}/health" >/dev/null 2>&1; then
    healthy=1
    break
  fi
  sleep 1
done
if [ "$healthy" -ne 1 ]; then
  log "pollis-delivery did not become healthy"
  cat "$RUN_DIR/pollis-delivery.log" >&2 || true
  exit 1
fi
log "pollis-delivery healthy"

# --- 4. emit the env the e2e scripts consume --------------------------------
# stdout: `export K=V` (evaluable locally). CI: also appended to $GITHUB_ENV so
# subsequent workflow steps (the e2e scripts) inherit it.
emit() {
  local key="$1" value="$2"
  echo "export ${key}=${value}"
  if [ -n "${GITHUB_ENV:-}" ]; then
    echo "${key}=${value}" >> "$GITHUB_ENV"
  fi
}
emit TURSO_URL "$TURSO_URL"
emit TURSO_TOKEN "$TURSO_TOKEN"
emit POLLIS_DELIVERY_URL "$POLLIS_DELIVERY_URL"
emit R2_S3_ENDPOINT "$R2_S3_ENDPOINT"
emit R2_PUBLIC_URL "$R2_PUBLIC_URL"
emit DEV_OTP "000000"
log "backend ready"
