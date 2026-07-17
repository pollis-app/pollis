#!/usr/bin/env bash
#
# Ephemeral LiveKit server for the media E2E (issue #570, M3a).
#
# A real call needs a LiveKit SFU: the callee learns of an incoming call over a
# LiveKit realtime data packet on its inbox room, and both peers join the
# `call-<ulid>` room to exchange (E2EE) audio. This stands up a throwaway
# livekit-server on loopback in DEV mode — which uses the well-known dev
# credentials API key `devkey` / secret `secret` — so no secret provisioning is
# needed for CI.
#
# It runs under Docker `--network host` (Linux runner) so both app instances
# reach it on 127.0.0.1:7880 (signaling/WS) and the RTC ports directly, and so
# LiveKit advertises host-local ICE candidates the loopback clients can use.
#
# Wiring (env var names verified against the code):
#   - the APP dials `LIVEKIT_URL` directly  (pollis-core/src/config.rs → the
#     `Room::connect(&url, ...)` in commands/voice/lifecycle.rs);
#   - the DELIVERY SERVICE mints room tokens with `LIVEKIT_API_KEY` /
#     `LIVEKIT_API_SECRET` / `LIVEKIT_URL`  (pollis-delivery/src/broker.rs
#     BrokerConfig::from_env / livekit_ready).
# All four are emitted to $GITHUB_ENV (CI) / stdout `export` lines (local) so the
# later start-backend.sh step's DS inherits the key/secret and the app step gets
# LIVEKIT_URL. Logging goes to stderr so stdout stays pure `export` lines.
#
# Teardown is folded into e2e/scripts/stop-backend.sh (removes the container).
set -euo pipefail

# Pinned by tag to match the production stack (livekit/docker-compose.yml).
LIVEKIT_IMAGE="${LIVEKIT_IMAGE:-livekit/livekit-server:v1.10.0}"
LIVEKIT_CONTAINER="${LIVEKIT_CONTAINER:-pollis-e2e-livekit}"
LIVEKIT_PORT="${LIVEKIT_PORT:-7880}"
# `--dev` mode's fixed credentials — LiveKit hard-codes these when run with the
# dev flag. The DS signs join tokens with them; the server verifies against them.
LK_API_KEY="${LIVEKIT_API_KEY:-devkey}"
LK_API_SECRET="${LIVEKIT_API_SECRET:-secret}"
# Loopback ws URL both the app and the DS use.
LK_URL="${LIVEKIT_URL:-ws://127.0.0.1:${LIVEKIT_PORT}}"

log() { echo "[start-livekit] $*" >&2; }

# Fresh container each run (host networking, so a leftover would bind :7880).
log "starting LiveKit ($LIVEKIT_IMAGE) in dev mode on 127.0.0.1:$LIVEKIT_PORT"
docker rm -f "$LIVEKIT_CONTAINER" >/dev/null 2>&1 || true
# --dev: dev credentials + loopback-friendly RTC config, no TLS/TURN/cert files
# (unlike the production livekit/livekit.yml). Host networking so the RTC UDP/TCP
# ports and ICE candidates are reachable from the two local app instances.
docker run -d --name "$LIVEKIT_CONTAINER" \
  --network host \
  "$LIVEKIT_IMAGE" --dev >/dev/null

# LiveKit answers `GET /` with a plain "OK" on its HTTP port once ready.
log "waiting for LiveKit to become healthy..."
healthy=0
for _ in $(seq 1 60); do
  if curl -fsS "http://127.0.0.1:${LIVEKIT_PORT}/" >/dev/null 2>&1; then
    healthy=1
    break
  fi
  sleep 1
done
if [ "$healthy" -ne 1 ]; then
  log "ERROR: LiveKit did not become healthy on :$LIVEKIT_PORT"
  docker logs "$LIVEKIT_CONTAINER" >&2 || true
  exit 1
fi
log "LiveKit healthy (dev key '$LK_API_KEY')"

# --- emit the env the app + DS consume --------------------------------------
emit() {
  local key="$1" value="$2"
  echo "export ${key}=${value}"
  if [ -n "${GITHUB_ENV:-}" ]; then
    echo "${key}=${value}" >> "$GITHUB_ENV"
  fi
}
emit LIVEKIT_URL "$LK_URL"
emit LIVEKIT_API_KEY "$LK_API_KEY"
emit LIVEKIT_API_SECRET "$LK_API_SECRET"
log "LiveKit ready"
