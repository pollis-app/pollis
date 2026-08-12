#!/usr/bin/env bash
#
# snapshot-state.sh — take a dated copy of the remote terraform state.
#
# R2 has no object versioning (PutBucketVersioning returns NotImplemented), so
# unlike an S3 backend there is no built-in history to roll back to after a bad
# apply or a corrupted write. This is that history: a server-side copy to a
# dated key in the same bucket, which costs one API call and no download.
#
# Run it before anything risky (a targeted apply, a state surgery, a provider
# major bump). Losing this state orphans ~116 resources across four regions —
# Terraform would no longer know they exist, so they would keep billing and a
# re-apply would build duplicates alongside them.
#
#   ./scripts/snapshot-state.sh
#   ./scripts/snapshot-state.sh --list
#
# Restore is a deliberate manual step, not a flag on this script: copy the chosen
# snapshot back over relay-hydra/terraform.tfstate with the AWS CLI, then run
# `terraform plan` and read it in full before applying anything.

set -euo pipefail

BUCKET="${TFSTATE_BUCKET:-pollis-tfstate}"
KEY="${TFSTATE_KEY:-relay-hydra/terraform.tfstate}"
ENDPOINT="${R2_ENDPOINT:-https://4bd9ab176c5febd5e7ac1b64b23dede5.r2.cloudflarestorage.com}"
export AWS_PROFILE="${TFSTATE_PROFILE:-r2-tfstate}"
export AWS_DEFAULT_REGION=auto

if [ "${1:-}" = "--list" ]; then
  aws s3 ls "s3://${BUCKET}/snapshots/" --endpoint-url "$ENDPOINT"
  exit 0
fi

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
DEST="s3://${BUCKET}/snapshots/terraform.tfstate.${STAMP}"

# Server-side copy: the state never touches this disk, so a snapshot cannot
# leak it into a shell history, a temp file, or a backup that outlives it.
aws s3 cp "s3://${BUCKET}/${KEY}" "$DEST" --endpoint-url "$ENDPOINT" >/dev/null
echo "snapshot -> ${DEST}"
