#!/usr/bin/env bash
# Publish rendered /learn media to R2 (cdn.pollis.com/learn/media/), then check the CDN serves it.
#
# Usage:  learn/manim/publish.sh <dir>
#   Uploads every <name>.mp4 / .webm / .jpg in <dir> as learn/media/<name>.<ext>.
#   Give re-renders a NEW name (e.g. <slug>-r1723.mp4) and point website/learn.html at it:
#   the CDN caches these objects immutably, so this script never overwrites an existing key.
#
# Endpoint and bucket follow docs/deployments.md → "/learn section": the bare account endpoint
# (R2_S3_ENDPOINT already ends in /pollis), bucket `pollis`, content type set explicitly.
# Credentials come from Doppler prd_prod. Run it BEFORE deploying a page that references the keys.
set -euo pipefail
dir="${1:?directory of rendered media}"
cd "$dir"
export AWS_ACCESS_KEY_ID="$(doppler secrets get R2_ACCESS_KEY_ID -p pollis -c prd_prod --plain)"
export AWS_SECRET_ACCESS_KEY="$(doppler secrets get R2_SECRET_KEY -p pollis -c prd_prod --plain)"
export AWS_DEFAULT_REGION=auto
ep="https://$(doppler secrets get CLOUDFLARE_ACCOUNT_ID -p pollis -c prd_prod --plain).r2.cloudflarestorage.com"

shopt -s nullglob
files=(*.mp4 *.webm *.jpg)
[ "${#files[@]}" -gt 0 ] || { echo "no .mp4/.webm/.jpg in $dir"; exit 1; }
for f in "${files[@]}"; do
  case "$f" in *.mp4) ct=video/mp4 ;; *.webm) ct=video/webm ;; *.jpg) ct=image/jpeg ;; esac
  key="learn/media/$f"
  if aws s3api head-object --endpoint-url "$ep" --bucket pollis --key "$key" >/dev/null 2>&1; then
    echo "exists, left alone: $key"
    continue
  fi
  aws s3api put-object --endpoint-url "$ep" --bucket pollis --key "$key" --body "$f" --content-type "$ct" >/dev/null
  echo "put: $key"
done

fail=0
for f in "${files[@]}"; do
  code="$(curl -s -o /dev/null -w '%{http_code}' "https://cdn.pollis.com/learn/media/$f")"
  echo "$code https://cdn.pollis.com/learn/media/$f"
  [ "$code" = 200 ] || fail=1
done
exit "$fail"
