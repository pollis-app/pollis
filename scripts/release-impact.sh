#!/usr/bin/env bash
# Release impact: for every deploy target, what has landed on main that the
# live deployment does not have yet (#1347).
#
# The post-merge release checklist in docs/deployments.md is the policy; this is
# the part of it a machine can answer. It reads what is LIVE (public version
# endpoints, release tags, GitHub run/deployment records), diffs that commit
# against main over the paths that feed the target, and prints one row per
# target. It never deploys, never blocks, and needs no secrets beyond `gh`.
#
#   scripts/release-impact.sh              # every target vs origin/main
#   scripts/release-impact.sh --pr 1346    # which targets one PR touches
#   scripts/release-impact.sh --brief      # stale targets only, one line each
#   scripts/release-impact.sh --ref <sha>  # compare against another commit
#
# "Live" is exact for the DS (/version returns the SHA) and for anything
# released from a tag. It is a proxy for the rest, and the row says so:
#   - mobile store builds: the latest mobile-v* tag (stores expose no commit)
#   - transparency log / relay image: the last successful workflow run's SHA
#   - LiveKit: the GitHub deployment record (the dispatch ref, not inputs.ref)
#   - website: main as of the last successful website-deploy run (the Pages
#     hook always builds main HEAD at that moment)
#
# Exit status is 0 whatever it finds: this reports, it does not gate.

set -uo pipefail

REPO_SLUG="${POLLIS_REPO:-pollis-app/pollis}"
REF="origin/main"
MODE="full"
PR=""

while [ $# -gt 0 ]; do
  case "$1" in
    --pr) PR="$2"; MODE="pr"; shift 2 ;;
    --brief) MODE="brief"; shift ;;
    --ref) REF="$2"; shift 2 ;;
    -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

cd "$(git rev-parse --show-toplevel)" || exit 0

# Bounded so a hung network call can never stall a hook.
with_timeout() { perl -e 'alarm shift; exec @ARGV' "$@"; }
http_json() { with_timeout 10 curl -fsSL "$1" 2>/dev/null; }

with_timeout 60 git fetch -q --tags origin main 2>/dev/null || true

# ---------------------------------------------------------------------------
# What feeds each target. Docs, tests and e2e flows never change what ships.
# CI workflow files are left out too: editing a workflow changes how the next
# release is built, not what the live one is running.
# ---------------------------------------------------------------------------
NOT_DOCS=(':(exclude,glob)**/*.md' ':(exclude).codesight'
          ':(exclude,glob)**/tests/**' ':(exclude,glob)**/benches/**'
          ':(exclude,glob)**/*.test.ts' ':(exclude,glob)**/*.test.tsx'
          ':(exclude)mobile/.maestro' ':(exclude)mobile/scripts' ':(exclude)e2e'
          ':(exclude)fuzz')

# pollis-core and everything it links, per the Cargo path-dependency graph.
# pollis-relay is in here: pollis-core depends on it, so a relay change reaches
# every client, not only the relay pool. patches/ is the root [patch] table
# (device-info, via livekit).
CORE=(pollis-core pollis-api pollis-relay pollis-device-cert pollis-schema
      pollis-sqlcipher verifiable-log-serve verifiable-log-builder verifiable-log
      pollis-libsql-connector patches Cargo.toml Cargo.lock rust-toolchain.toml
      .cargo/config.toml)

DS=(pollis-delivery pollis-api pollis-schema pollis-device-cert
    pollis-libsql-connector Cargo.toml Cargo.lock rust-toolchain.toml
    .github/scripts/sync-ds-secrets.sh scripts/db-apply.sh)
DESKTOP=(src-tauri frontend pollis-capture-proto pollis-capture-linux
         pollis-capture-macos "${CORE[@]}")
CLI=(pollis-tui "${CORE[@]}")
# Native = anything in the mobile fingerprint: it needs a store build + APK and
# can never ride an OTA. JS = what an OTA can carry.
MOBILE_NATIVE=("${CORE[@]}" mobile/modules mobile/plugins mobile/patches
               mobile/app.config.js mobile/app.json mobile/package.json
               mobile/pnpm-lock.yaml mobile/store/ota-code-signing.pem
               mobile/fingerprint.config.js)
MOBILE_ALL=(mobile frontend/src/i18n/locales "${CORE[@]}")
VERIFIER=(verifiable-log-serve verifiable-log-builder verifiable-log pollis-relay
          pollis-device-cert pollis-libsql-connector Cargo.lock)
TLOG=(verifiable-log-serve verifiable-log-builder verifiable-log pollis-relay
      pollis-device-cert pollis-libsql-connector Cargo.lock key-set.json)
LIVEKIT=(livekit)
WEBSITE=(website)
# Internal sources that public pages are reconciled against (website-verify.yml
# checks the live site against these).
DOC_SOURCES=(docs/metadata-retention-policy.md docs/operational-continuity.md
             mobile/store/android-sideload-cert.sha256 key-set.json README.md
             website/rebuild-ledger.json)
RELAY=(pollis-relay pollis-device-cert verifiable-log Cargo.lock)

# ---------------------------------------------------------------------------
# Where each target's live commit comes from
# ---------------------------------------------------------------------------
tag_commit() { git rev-list -n1 "$1" 2>/dev/null; }

latest_tag() { git tag -l "$1" --sort=-v:refname | head -1; }

ds_sha() { http_json "$1/version" | jq -r '.sha // empty' 2>/dev/null; }

last_run_sha() {
  with_timeout 20 gh run list -R "$REPO_SLUG" -w "$1" -s success -L1 \
    --json headSha -q '.[0].headSha // empty' 2>/dev/null
}

deployment_sha() {
  with_timeout 20 gh api "repos/$REPO_SLUG/deployments?environment=$1&per_page=1" \
    --jq '.[0].sha // empty' 2>/dev/null
}

website_sha() {
  local at
  at="$(with_timeout 20 gh run list -R "$REPO_SLUG" -w website-deploy.yml -s success -L1 \
    --json createdAt -q '.[0].createdAt // empty' 2>/dev/null)"
  [ -n "$at" ] && git rev-list -1 --before="$at" "$REF" 2>/dev/null
}

# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------
pending() {
  # pending <live-sha> <paths...> → one "<sha> <subject>" line per commit. A
  # commit whose only change in these paths is Cargo.lock is tagged [deps]: it
  # moved some dependency in the workspace, which may or may not be one this
  # target links (`cargo tree -p <crate>` answers that), so it is reported
  # separately and does not by itself mark the target stale.
  local live="$1" c subj; shift
  git log --format='%h %s' "$live..$REF" -- "$@" "${NOT_DOCS[@]}" 2>/dev/null |
  while read -r c subj; do
    if [ -z "$(git diff-tree --no-commit-id --name-only -r "$c" -- "$@" ':(exclude)Cargo.lock' "${NOT_DOCS[@]}")" ]; then
      echo "$c [deps] $subj"
    else
      echo "$c $subj"
    fi
  done
}

row() {
  # row <target> <live-label> <live-sha> <how-to-ship> <paths...>
  local target="$1" label="$2" live="$3" ship="$4"; shift 4
  local log count deps extra
  if [ -z "$live" ] || ! git cat-file -e "$live^{commit}" 2>/dev/null; then
    [ "$MODE" = brief ] && { echo "?  $target: live version unknown ($label)"; return; }
    printf '%-22s %-8s %s\n' "$target" "UNKNOWN" "could not read live version ($label)"
    return
  fi
  log="$(pending "$live" "$@")"
  count="$(printf '%s' "$log" | grep -vc '^[0-9a-f]* \[deps\]' || true)"
  deps="$(printf '%s' "$log" | grep -c '^[0-9a-f]* \[deps\]' || true)"
  if [ "$count" -eq 0 ] && [ "$deps" -gt 0 ]; then
    [ "$MODE" = brief ] && return
    printf '%-22s %-8s %s @ %s, %d lockfile-only change(s): check `cargo tree` before shipping\n' \
      "$target" "DEPS?" "$label" "${live:0:8}" "$deps"
    printf '%s\n' "$log" | head -4 | sed 's/^/                        /'
    return
  fi
  if [ "$count" -eq 0 ]; then
    [ "$MODE" = brief ] && return
    printf '%-22s %-8s %s @ %s\n' "$target" "current" "$label" "${live:0:8}"
    return
  fi
  if [ "$MODE" = brief ]; then
    extra=""
    [ "$deps" -gt 0 ] && extra=" (+$deps lockfile-only)"
    echo "!  $target: $count change(s) not shipped$extra (live $label @ ${live:0:8}) → $ship"
    return
  fi
  printf '%-22s %-8s %s @ %s, %d change(s) behind → %s\n' \
    "$target" "STALE" "$label" "${live:0:8}" "$((count + deps))" "$ship"
  printf '%s\n' "$log" | head -8 | sed 's/^/                        /'
  [ "$((count + deps))" -gt 8 ] && echo "                        … and $((count + deps - 8)) more"
}

# A PR's blast radius: which targets do its changed files feed?
pr_mode() {
  local merge base head files
  merge="$(gh pr view "$PR" -R "$REPO_SLUG" --json mergeCommit -q '.mergeCommit.oid // empty' 2>/dev/null)"
  if [ -n "$merge" ]; then
    with_timeout 60 git fetch -q origin "$merge" 2>/dev/null || true
    base="$merge^1"; head="$merge"
  else
    with_timeout 60 git fetch -q origin "pull/$PR/head" 2>/dev/null || { echo "cannot fetch PR $PR" >&2; exit 0; }
    head="$(git rev-parse FETCH_HEAD)"; base="$(git merge-base "$REF" "$head")"
  fi
  echo "PR #$PR ($(git rev-parse --short "$head")) feeds:"
  hit() {
    local name="$1" ship="$2"; shift 2
    files="$(git diff --name-only "$base" "$head" -- "$@" "${NOT_DOCS[@]}" 2>/dev/null)"
    [ -z "$files" ] && return
    printf '  %-22s → %s\n' "$name" "$ship"
    printf '%s\n' "$files" | head -5 | sed 's/^/      /'
  }
  hit "dev DS" "delivery-deploy-dev.yml" "${DS[@]}"
  hit "prod DS" "delivery-deploy-prod.yml (after dev verifies)" "${DS[@]}"
  hit "desktop" "tag v* (desktop-release.yml)" "${DESKTOP[@]}"
  hit "Pollis CLI" "tag v* (cli-release.yml)" "${CLI[@]}"
  hit "mobile native" "store builds + mobile-v* tag (APK)" "${MOBILE_NATIVE[@]}"
  hit "mobile JS" "OTA (mobile-ota-*) or store build" mobile frontend/src/i18n/locales
  hit "pollis-verify" "tag pollis-verify-v*" "${VERIFIER[@]}"
  hit "transparency log" "transparency-publish.yml (or the daily cron)" "${TLOG[@]}"
  hit "LiveKit" "livekit-deploy.yml" "${LIVEKIT[@]}"
  hit "website" "website-deploy.yml" "${WEBSITE[@]}"
  hit "relay image" "relay-image.yml (pool currently off)" "${RELAY[@]}"
  files="$(git diff --name-only "$base" "$head" -- 'pollis-core/src' 2>/dev/null)"
  [ -n "$files" ] && echo "  note: pollis-core changed. If a wire contract moved (signing, DS request/response shapes, r2 presign, livekit_jwt, MLS envelope), the DS re-implements it by hand: mirror it in pollis-delivery and redeploy."
  files="$(git diff --name-only --diff-filter=A "$base" "$head" -- pollis-schema/migrations pollis-schema/migrations-log 2>/dev/null)"
  [ -n "$files" ] && echo "  note: new migration(s). Prod applies them on the first DS deploy or desktop release, whichever runs first."
  files="$(git diff --name-only "$base" "$head" -- frontend/src mobile/app mobile/components mobile/hooks pollis-tui/src 2>/dev/null)"
  [ -n "$files" ] && echo "  parity: client code changed; run node scripts/feature-parity.mjs --since $(git rev-parse --short "$base")"
  exit 0
}

[ "$MODE" = pr ] && pr_mode

# ---------------------------------------------------------------------------
# Gather live versions
# ---------------------------------------------------------------------------
dev_ds="$(ds_sha https://api-dev.pollis.com)"
prod_ds="$(ds_sha https://api.pollis.com)"

desktop_v="$(http_json https://cdn.pollis.com/releases/latest.json | jq -r '.version // empty' 2>/dev/null)"
desktop_sha="$(tag_commit "$desktop_v")"

cli_v="$(http_json https://cdn.pollis.com/releases/cli/latest.json | jq -r '.version // empty' 2>/dev/null)"
cli_sha="$(tag_commit "v$cli_v")"

apk_v="$(http_json https://cdn.pollis.com/releases/android/latest.json | jq -r '.version // empty' 2>/dev/null)"
apk_sha="$(tag_commit "mobile-v$apk_v")"
store_tag="$(latest_tag 'mobile-v*')"
store_sha="$(tag_commit "$store_tag")"

# OTA builds on top of whichever is newer: the last store build or the last OTA.
ota_tag="$(git for-each-ref --sort=-creatordate --format='%(refname:short)' 'refs/tags/mobile-ota-*' | head -1)"
ota_base_sha="$store_sha"; ota_label="$store_tag (no OTA since)"
if [ -n "$ota_tag" ]; then
  o="$(tag_commit "$ota_tag")"
  if [ -n "$o" ] && [ -n "$store_sha" ] && git merge-base --is-ancestor "$store_sha" "$o" 2>/dev/null; then
    ota_base_sha="$o"; ota_label="$ota_tag"
  fi
fi

verify_tag="$(latest_tag 'pollis-verify-v*')"
verify_sha="$(tag_commit "$verify_tag")"
tlog_sha="$(last_run_sha transparency-publish.yml)"
livekit_sha="$(deployment_sha livekit-prod)"
web_sha="$(website_sha)"
relay_sha="$(last_run_sha relay-image.yml)"

echo "Release impact vs ${REF} ($(git rev-parse --short "$REF"))"
[ "$MODE" = brief ] || echo

row "dev DS"           "/version"          "$dev_ds"      "gh workflow run delivery-deploy-dev.yml -f ref=main"     "${DS[@]}"
row "prod DS"          "/version"          "$prod_ds"     "delivery-deploy-prod.yml after dev verifies"             "${DS[@]}" pollis-delivery/worker/updates.ts
row "desktop"          "${desktop_v:-?}"   "$desktop_sha" "tag v* (desktop-release.yml)"                             "${DESKTOP[@]}"
row "Pollis CLI"       "v${cli_v:-?}"      "$cli_sha"     "tag v* (cli-release.yml, same tag as desktop)"           "${CLI[@]}"
row "mobile store+APK" "${store_tag:-?}"   "$store_sha"   "bump mobile/package.json, store builds, tag mobile-v*"   "${MOBILE_ALL[@]}"
if [ -n "$apk_sha" ] && [ "$apk_sha" != "$store_sha" ]; then
  row "sideload APK"   "mobile-v${apk_v}"  "$apk_sha"     "the APK lags the store tag: re-run mobile-apk-release.yml" "${MOBILE_ALL[@]}"
fi
# OTA can only carry JS. If native code changed since the store build, the fix
# has to be a store build instead, and the row says so.
if [ "$MODE" != brief ] && [ -n "$store_sha" ]; then
  native="$(pending "$store_sha" "${MOBILE_NATIVE[@]}" | grep -c . || true)"
  [ "$native" -gt 0 ] && echo "                        ($native native change(s) since $store_tag: these need a store build, an OTA cannot carry them)"
fi
row "mobile OTA (JS)"  "$ota_label"        "$ota_base_sha" "OTA publish (mobile-ota-*) if JS-only, else store build" mobile frontend/src/i18n/locales ":(exclude)mobile/modules" ":(exclude)mobile/plugins" ":(exclude)mobile/patches" ":(exclude)mobile/package.json" ":(exclude)mobile/pnpm-lock.yaml" ":(exclude)mobile/app.config.js" ":(exclude)mobile/app.json" ":(exclude)mobile/tests" ":(exclude)mobile/.maestro" ":(exclude)mobile/scripts"
row "pollis-verify"    "${verify_tag:-?}"  "$verify_sha"  "tag pollis-verify-v* (verifier-release.yml)"             "${VERIFIER[@]}"
row "transparency log" "last publish run"  "$tlog_sha"    "transparency-publish.yml (daily cron also picks it up)"  "${TLOG[@]}"
row "LiveKit"          "livekit-prod deployment" "$livekit_sha" "gh workflow run livekit-deploy.yml -f environment=prod" "${LIVEKIT[@]}"
row "website"          "main at last deploy" "$web_sha"   "gh workflow run website-deploy.yml"                      "${WEBSITE[@]}"
row "public docs"      "main at last deploy" "$web_sha"   "reconcile website/ pages with these sources, then deploy" "${DOC_SOURCES[@]}"
row "relay image"      "last image run"    "$relay_sha"   "relay-image.yml (pool currently off)"                     "${RELAY[@]}"

if [ "$MODE" != brief ]; then
  echo
  # Prod migrations run on the first DS deploy or desktop release. Whatever is
  # missing from BOTH of those commits has not been applied to prod yet.
  pend=""
  for f in $(git ls-tree -r --name-only "$REF" -- pollis-schema/migrations pollis-schema/migrations-log | grep '\.sql$'); do
    in_ds=1; in_desk=1
    { [ -z "$prod_ds" ] || ! git cat-file -e "$prod_ds:$f" 2>/dev/null; } && in_ds=0
    { [ -z "$desktop_sha" ] || ! git cat-file -e "$desktop_sha:$f" 2>/dev/null; } && in_desk=0
    [ "$in_ds" -eq 0 ] && [ "$in_desk" -eq 0 ] && pend="$pend $f"
  done
  if [ -n "$pend" ]; then
    echo "Prod migrations pending (ship with the next prod DS deploy or desktop release):"
    for f in $pend; do echo "  $f"; done
  else
    echo "Prod migrations: none pending."
  fi
  wv="$(with_timeout 20 gh run list -R "$REPO_SLUG" -w website-verify.yml -L1 --json conclusion,createdAt -q '.[0] | "\(.conclusion) (\(.createdAt))"' 2>/dev/null)"
  echo "Live-site doc checks (website-verify.yml, last run): ${wv:-unknown}"
  echo
  echo "Feature parity across desktop, mobile and CLI: node scripts/feature-parity.mjs"
fi
exit 0
