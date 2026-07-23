#!/usr/bin/env bash
# Mint the Ed25519 directory-signing keypair (issue #616, §4/§9).
#
#   - Private key  -> SSM SecureString (reconciler-only).
#   - Public key   -> printed as POLLIS_OVERLAY_DIRECTORY_KEY (base64 of the raw
#                     32 bytes). Hand this to the client build.
#
# Run this FIRST (§9 sequencing): the public key drops out immediately so the
# client build can proceed in parallel with the rest of the infra. Safe to run
# before `terraform apply` — the SSM param is created here, not by Terraform.
#
# Usage: scripts/mint-signing-key.sh [region] [param-name]
set -euo pipefail

REGION="${1:-us-west-2}"
PARAM="${2:-/pollis/relay-hydra/signing-key}"

command -v openssl >/dev/null || { echo "openssl required" >&2; exit 1; }
command -v aws >/dev/null || { echo "aws CLI required" >&2; exit 1; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
PRIV="$TMP/signing.pem"

openssl genpkey -algorithm ed25519 -out "$PRIV"

# The SPKI DER for Ed25519 is 44 bytes; the raw 32-byte public key is the tail.
PUB_B64="$(openssl pkey -in "$PRIV" -pubout -outform DER | tail -c 32 | base64 -w0)"

aws ssm put-parameter \
  --region "$REGION" \
  --name "$PARAM" \
  --type SecureString \
  --overwrite \
  --value "$(cat "$PRIV")" \
  --description "Pollis relay directory signing key (Ed25519 private, PKCS8 PEM)" >/dev/null

echo "Stored private signing key in SSM: $PARAM ($REGION)"
echo
echo "Hand this to the client build:"
echo "  POLLIS_OVERLAY_DIRECTORY_KEY=$PUB_B64"
