#!/usr/bin/env bash
# Sync the Delivery Service secrets from Doppler into the Cloudflare Wrangler
# Secrets Store, one source of truth -> runtime (#515). Called by the
# delivery-deploy-{dev,prod} workflows from the pollis-delivery/ directory.
#
# Env in:
#   DOPPLER_TOKEN          service token scoped to the env's Doppler config
#   STORE_ID               Secrets Store id (account has a single store)
#   SECRET_PREFIX          DS_DEV_ or DS_PROD_ (namespaces dev/prod in one store)
#   INCLUDE_DEV_OTP        "true" to also sync DEV_OTP (dev only)
#   CLOUDFLARE_API_TOKEN / CLOUDFLARE_ACCOUNT_ID   for wrangler
set -euo pipefail

: "${DOPPLER_TOKEN:?}" "${STORE_ID:?}" "${SECRET_PREFIX:?}"

# Keys the DS reads (pollis-delivery/src: main.rs direct reads + broker.rs
# from_env). All optional in the DS except TURSO_URL/TURSO_TOKEN; a key unset in
# Doppler is skipped so it never overwrites the store with an empty value.
KEYS=(
  TURSO_URL TURSO_TOKEN
  LOG_DB_URL LOG_DB_ADMIN_TOKEN
  RESEND_API_KEY
  LIVEKIT_API_KEY LIVEKIT_API_SECRET LIVEKIT_URL
  R2_S3_ENDPOINT R2_ACCESS_KEY_ID R2_SECRET_KEY R2_BUCKET
  TURSO_PLATFORM_TOKEN TURSO_ORG TURSO_DB
)
if [ "${INCLUDE_DEV_OTP:-false}" = "true" ]; then
  KEYS+=(DEV_OTP)
fi

SECRETS_JSON="$(doppler secrets download --no-file --format json)"

upsert() {
  local name="$1" value="$2"
  # Mask so the value never lands in workflow logs.
  echo "::add-mask::$value"
  # Upsert: update if it exists, else create. Scope to workers (the only reader).
  if pnpm exec wrangler secrets-store secret update "$STORE_ID" \
        --name "$name" --value "$value" --scopes workers --remote >/dev/null 2>&1; then
    echo "updated $name"
  else
    pnpm exec wrangler secrets-store secret create "$STORE_ID" \
        --name "$name" --value "$value" --scopes workers --remote >/dev/null
    echo "created $name"
  fi
}

for key in "${KEYS[@]}"; do
  value="$(printf '%s' "$SECRETS_JSON" | jq -r --arg k "$key" '.[$k] // empty')"
  if [ -z "$value" ]; then
    echo "skip $key (unset in Doppler)"
    continue
  fi
  upsert "${SECRET_PREFIX}${key}" "$value"
done

echo "Secrets sync complete (prefix ${SECRET_PREFIX})."
