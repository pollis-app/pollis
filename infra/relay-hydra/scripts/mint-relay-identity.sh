#!/usr/bin/env bash
# Mint the ONE shared pool QUIC identity (issue #616, §4/§9).
#
# Runs the published relay image once so it generates identity.key + its
# self-signed leaf cert (identity.key.crt, raw DER), then stores BOTH in SSM
# SecureStrings (base64). Every node fetches these at boot; the client pins the
# cert (published as cert_b64 in the directory). Prints cert_b64 for reference.
#
# Usage: scripts/mint-relay-identity.sh [region] [image]
set -euo pipefail

REGION="${1:-us-west-2}"
IMAGE="${2:-ghcr.io/actuallydan/pollis-relay:latest}"
KEY_PARAM="/pollis/relay-hydra/identity-key"
CERT_PARAM="/pollis/relay-hydra/identity-cert"

command -v docker >/dev/null || { echo "docker required" >&2; exit 1; }
command -v aws >/dev/null || { echo "aws CLI required" >&2; exit 1; }

TMP="$(mktemp -d)"
CNAME="pollis-relay-mint-$$"
cleanup() { docker rm -f "$CNAME" >/dev/null 2>&1 || true; rm -rf "$TMP"; }
trap cleanup EXIT

docker pull "$IMAGE" >/dev/null

# Start the relay just long enough to generate the identity on first boot, then
# stop it. The allowlist/bind are throwaway — we only want the generated files.
docker run -d --name "$CNAME" -v "$TMP:/id" "$IMAGE" \
  --identity /id/identity.key --bind 0.0.0.0:9444 --allow example.invalid >/dev/null

echo -n "waiting for identity generation"
for _ in $(seq 1 30); do
  if [ -s "$TMP/identity.key" ] && [ -s "$TMP/identity.key.crt" ]; then
    break
  fi
  echo -n "."
  sleep 1
done
echo

if [ ! -s "$TMP/identity.key" ] || [ ! -s "$TMP/identity.key.crt" ]; then
  echo "identity files were not generated — check the image and its --identity flag" >&2
  exit 1
fi

KEY_B64="$(base64 -w0 "$TMP/identity.key")"
CERT_B64="$(base64 -w0 "$TMP/identity.key.crt")"

aws ssm put-parameter --region "$REGION" --name "$KEY_PARAM" --type SecureString \
  --overwrite --value "$KEY_B64" \
  --description "Pollis relay pool QUIC identity key (base64 raw)" >/dev/null
aws ssm put-parameter --region "$REGION" --name "$CERT_PARAM" --type SecureString \
  --overwrite --value "$CERT_B64" \
  --description "Pollis relay pool QUIC leaf cert (base64 DER) — pinned by clients" >/dev/null

echo "Stored pool QUIC identity in SSM: $KEY_PARAM, $CERT_PARAM ($REGION)"
echo
echo "cert_b64 (also embedded in each directory relay entry):"
echo "  $CERT_B64"
