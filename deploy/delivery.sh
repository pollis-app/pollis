#!/usr/bin/env bash
#
# Recreate a Pollis Delivery Service container on the VPS with fresh env pulled
# from Doppler. This is the ONE place the DS runtime env is injected.
#
# Division of labor (keeps secrets off GitHub — see delivery-deploy-*.yml):
#   - CODE deploys  → GitHub builds+pushes the image and pokes Watchtower, which
#     recreates the container *preserving* the env this script set. No secrets
#     ever touch GitHub.
#   - ENV / SECRET changes (rare) → run THIS script on the box. It re-reads
#     Doppler and recreates the container with the full, current env. After that,
#     Watchtower carries the env forward across future code deploys.
#
# Requires, on the VPS: docker, and doppler configured with a service token that
# can read the pollis project (`doppler configure set token <tok>`).
#
# Usage (on the VPS):   ./deploy/delivery.sh dev|prod
set -euo pipefail

ENV_NAME="${1:-}"
case "$ENV_NAME" in
  dev)  DCONF=dev_personal; NAME=delivery-dev; TAG=dev;  URL=https://api-dev.pollis.com ;;
  prod) DCONF=prd_prod;     NAME=delivery;     TAG=prod; URL=https://api.pollis.com ;;
  *) echo "usage: $0 dev|prod" >&2; exit 2 ;;
esac

IMAGE="ghcr.io/actuallydan/pollis-delivery:${TAG}"
NETWORK=livekit_default

# Env vars forwarded into the container. Values come from `doppler run` (never
# from the command line / process args). Keep in sync with pollis-delivery's
# Config::from_env + BrokerConfig::from_env.
ENV_KEYS=(
  TURSO_URL TURSO_TOKEN LOG_DB_URL LOG_DB_ADMIN_TOKEN
  RESEND_API_KEY POLLIS_DS_REQUIRE_AUTH PORT
  LIVEKIT_API_KEY LIVEKIT_API_SECRET LIVEKIT_URL
  R2_S3_ENDPOINT R2_ACCESS_KEY_ID R2_SECRET_KEY R2_BUCKET
  TURSO_PLATFORM_TOKEN TURSO_ORG TURSO_DB
)
[ "$ENV_NAME" = "dev" ] && ENV_KEYS+=(DEV_OTP)

EFLAGS=()
for k in "${ENV_KEYS[@]}"; do EFLAGS+=(-e "$k"); done

echo "▶ pulling $IMAGE"
docker pull "$IMAGE"

echo "▶ recreating $NAME on $NETWORK with fresh env from Doppler ($DCONF)"
docker rm -f "$NAME" >/dev/null 2>&1 || true
# `doppler run` populates the environment; `docker run -e KEY` (no value) forwards
# each var from that environment into the container — no secret on the argv.
doppler run --project pollis --config "$DCONF" --silent -- \
  docker run -d \
    --name "$NAME" \
    --network "$NETWORK" \
    --expose 8788 \
    --restart unless-stopped \
    --label com.centurylinklabs.watchtower.enable=true \
    "${EFLAGS[@]}" \
    "$IMAGE"

echo "▶ verifying $URL/version is serving the new build"
for i in $(seq 1 30); do
  sha="$(curl -fsS -m 8 "$URL/version" 2>/dev/null | jq -r '.sha' 2>/dev/null || echo '')"
  if [ -n "$sha" ] && [ "$sha" != "null" ]; then
    echo "✓ $NAME is live — /version sha=$sha"
    docker image prune -f >/dev/null 2>&1 || true
    exit 0
  fi
  sleep 4
done

echo "✗ $NAME did not answer /version within ~120s — recent logs:" >&2
docker logs "$NAME" --tail 40 2>&1 || true
exit 1
