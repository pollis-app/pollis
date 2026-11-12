#!/usr/bin/env bash
# publish-oss-evidence.sh <product> <version> <dir>
#
# Uploads one product release's open-source evidence (#1832), written by
# `node scripts/oss-inventory.mjs <product> --version <version> --out-dir <dir>`,
# to the public release bucket, then confirms the CDN serves it:
#
#   releases/oss/<product>/<version>/pollis-<product>-<version>.cdx.json   SBOM
#   releases/oss/<product>/<version>/pollis-<product>-<version>.vex.json   VEX
#   releases/oss/<product>/<version>/summary.json                          index
#   releases/oss/<product>/latest.json   = that summary (no-cache), what
#                                          pollis.com/open-source reads
#
# Versioned objects are never pruned (the desktop prune only touches
# releases/v*/). Needs AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY (the R2 write
# key) and CLOUDFLARE_ACCOUNT_ID in the environment, as the release jobs set them.
set -euo pipefail

product="${1:?product}"
version="${2:?version}"
dir="${3:?dir}"

case "$product" in
  desktop | mobile | cli | ds) ;;
  *) echo "::error::unknown product ${product}" >&2; exit 1 ;;
esac
# The version becomes an object key: refuse anything that is not a plain token.
if ! [[ "$version" =~ ^[A-Za-z0-9._-]+$ ]]; then
  echo "::error::refusing version '${version}'" >&2
  exit 1
fi

sbom="${dir}/pollis-${product}-${version}.cdx.json"
vex="${dir}/pollis-${product}-${version}.vex.json"
summary="${dir}/summary.json"
for f in "$sbom" "$vex" "$summary"; do
  [ -s "$f" ] || { echo "::error::missing ${f}" >&2; exit 1; }
done
# Never publish an empty claim (desktop-release.yml's SBOM guard, for every product).
n="$(jq '.components | length' "$sbom")"
if [ "${n:-0}" -lt 50 ]; then
  echo "::error::${sbom} lists only ${n} components; the generator did not see the tree" >&2
  exit 1
fi
jq -e --arg v "$version" --arg p "$product" '.version == $v and .product == $p' "$summary" >/dev/null

ENDPOINT="https://${CLOUDFLARE_ACCOUNT_ID:?}.r2.cloudflarestorage.com"
prefix="s3://pollis/releases/oss/${product}"
put() {
  aws s3 cp "$1" "$2" --endpoint-url "$ENDPOINT" --content-type application/json "${@:3}"
}
put "$sbom" "${prefix}/${version}/$(basename "$sbom")"
put "$vex" "${prefix}/${version}/$(basename "$vex")"
put "$summary" "${prefix}/${version}/summary.json"
put "$summary" "${prefix}/latest.json" --cache-control no-cache

# "Workflow green" is not "live": read back what the CDN serves.
cdn="https://cdn.pollis.com/releases/oss/${product}"
for i in 1 2 3 4 5 6; do
  served="$(curl -fsSL -H 'Cache-Control: no-cache' "${cdn}/latest.json" | jq -r .version || true)"
  if [ "$served" = "$version" ]; then
    break
  fi
  echo "CDN latest.json says ${served:-nothing}, want ${version}; retry ${i}"
  sleep 10
done
if [ "$served" != "$version" ]; then
  echo "::error::${cdn}/latest.json does not serve ${version}" >&2
  exit 1
fi
for f in "$sbom" "$vex"; do
  want="$(sha256sum "$f" | cut -d' ' -f1)"
  got="$(curl -fsSL "${cdn}/${version}/$(basename "$f")" | sha256sum | cut -d' ' -f1)"
  if [ "$got" != "$want" ]; then
    echo "::error::${cdn}/${version}/$(basename "$f") does not match what was uploaded" >&2
    exit 1
  fi
done
echo "published ${product} ${version}: ${n} components, ${cdn}/latest.json live"
