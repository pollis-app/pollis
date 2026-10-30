#!/usr/bin/env bash
# Post-deploy check (#1611, #1624): did POLLIS_CLIENT_POLICY_URL reach the DS container?
#
#   scripts/verify-client-policy.sh <base-url>        e.g. https://api.pollis.com
#   DEV_KEY=…  adds the dev gate's X-Pollis-Dev-Key header (api-dev).
#
# GET /v1/client-policy answers 404 only when the DS has no POLLIS_CLIENT_POLICY_URL: the same
# "config never crossed the Worker->container boundary" tell as #760. 200 (mirrored) or 503
# (configured, first fetch pending) passes.
#
# A deploy can leave the container running with the previous start's environment. The Worker
# (pollis-delivery/worker/container-env.ts) checks every request until it sees a running container
# started with the current environment, and restarts a stale one. So this script's own requests
# trigger the check: a 404 is answered by the stale container before a request finds it, and a
# request that triggers the restart blocks for the cold start (or times out as 000). Both are
# retried until WINDOW_SECONDS has passed; a 404 still there after the window fails the deploy.
#
# The window (#1624): deploy -> new image serving took 11-90s over 40 prod deploys and 15-128s over
# 40 dev deploys (each a full container cold start, as the env restart is), and the 2026-09-04 prod
# roll took ~3 min. 6 minutes is twice that worst case. Each request may block up to
# CURL_TIMEOUT_SECONDS, long enough to see a cold start through instead of abandoning it.
set -uo pipefail

BASE="${1:?usage: verify-client-policy.sh <base-url>}"
WINDOW_SECONDS="${WINDOW_SECONDS:-360}"
CURL_TIMEOUT_SECONDS="${CURL_TIMEOUT_SECONDS:-60}"
INTERVAL_SECONDS="${INTERVAL_SECONDS:-6}"
URL="${BASE%/}/v1/client-policy"

headers=()
if [ -n "${DEV_KEY:-}" ]; then
  headers=(-H "X-Pollis-Dev-Key: $DEV_KEY")
fi

start=$SECONDS
code=000
while :; do
  code="$(curl -s -o /dev/null -w '%{http_code}' -m "$CURL_TIMEOUT_SECONDS" ${headers[@]+"${headers[@]}"} "$URL" || true)"
  # curl prints 000 itself on a timeout or refused connection; guard an empty or odd reply too.
  [[ "$code" =~ ^[0-9]{3}$ ]] || code=000
  elapsed=$((SECONDS - start))
  case "$code" in
    200|503)
      echo "✓ /v1/client-policy answers $code after ${elapsed}s: POLLIS_CLIENT_POLICY_URL reached the DS."
      exit 0
      ;;
  esac
  echo "[${elapsed}s] /v1/client-policy -> $code"
  if [ "$elapsed" -ge "$WINDOW_SECONDS" ]; then
    break
  fi
  sleep "$INTERVAL_SECONDS"
done

if [ "$code" = 404 ]; then
  echo "::error::/v1/client-policy still answers 404 after ${elapsed}s: POLLIS_CLIENT_POLICY_URL did not reach the container, and the Worker's env reconcile (container-env.ts) did not restart it."
else
  echo "::error::/v1/client-policy answers $code after ${elapsed}s (want 200 or 503): the DS did not come back with the deployed environment."
fi
exit 1
