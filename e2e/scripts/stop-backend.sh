#!/usr/bin/env bash
#
# Tear down the backend fixtures brought up by e2e/scripts/start-backend.sh
# (M1 of #570): the pollis-delivery process and the libsql server container.
#
# Idempotent — every step is `|| true`, so it's safe to run in an `if: always()`
# CI step whether or not start-backend.sh got as far as starting anything.
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
RUN_DIR="${POLLIS_E2E_RUN_DIR:-$ROOT/e2e/.backend}"
LIBSQL_CONTAINER="${LIBSQL_CONTAINER:-pollis-e2e-libsql}"

log() { echo "[stop-backend] $*" >&2; }

# pollis-delivery: kill by recorded PID, then a name-based sweep as a backstop
# (mirrors the reap set in e2e/lib/harness.js / the desktop-e2e composite action).
if [ -f "$RUN_DIR/pollis-delivery.pid" ]; then
  DS_PID="$(cat "$RUN_DIR/pollis-delivery.pid" 2>/dev/null || true)"
  if [ -n "${DS_PID:-}" ]; then
    log "stopping pollis-delivery (pid $DS_PID)"
    kill "$DS_PID" >/dev/null 2>&1 || true
  fi
  rm -f "$RUN_DIR/pollis-delivery.pid" || true
fi
pkill -9 -f "target/debug/pollis-delivery" >/dev/null 2>&1 || true

# libsql server container.
log "removing libsql container ($LIBSQL_CONTAINER)"
docker rm -f "$LIBSQL_CONTAINER" >/dev/null 2>&1 || true

log "backend stopped"
