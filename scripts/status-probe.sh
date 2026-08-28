#!/usr/bin/env bash
# Probe the public Pollis endpoints and append the result to
# website/status-history.json — the durable record /status renders (#877).
#
# WHY THIS EXISTS. `/health` and `/version` have been open on every service for
# months and nothing published them, so there was no answer to "is it down, or
# is it being withheld?" — and on a product like this, silence during an outage
# reads as compromise. Worse, an outage here is not purely an availability
# question: a device that cannot reach the DS cannot report a watermark, and
# under #720 a device silent past POLLIS_DS_WATERMARK_STALE_MONTHS (12) stops
# pinning envelope retention. A long enough gap is a CORRECTNESS event, so it
# has to leave a permanent, dated record rather than a graph that resets.
#
# WHY A COMMITTED FILE. Same reasoning as website/rebuild-ledger.json: the site
# is static (Cloudflare Pages) and the DS sends no CORS headers, so the page
# cannot probe anything from a visitor's browser — a client-side fetch would
# fail on every load and render a false red. A committed file is the one record
# that is durable, diffable, and independently checkable in git history.
#
# WHAT IT DOES NOT DO. It is not a second-by-second uptime monitor and does not
# pretend to be: it samples on a schedule, from one GitHub-hosted runner, in one
# network location. `status.html` says so in those words.
#
# APPEND POLICY. A sample is appended when the observed state CHANGES, and
# otherwise once every HEARTBEAT_HOURS. The heartbeat is load-bearing: without
# it "no new samples" would be indistinguishable from "the prober is dead",
# which is the same conflation this file exists to remove.
#
# Usage:
#   scripts/status-probe.sh            # probe; append if changed or heartbeat due
#   scripts/status-probe.sh --check    # exit 1 if the newest sample is stale
#
# Requires: curl, jq. `gh` is optional — without it the transparency-publisher
# run history is recorded as unknown rather than guessed at.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO_ROOT}/website/status-history.json"

# How long a run of identical observations may go unrecorded. Two missed
# heartbeats is what `--check` treats as "the prober itself is down".
HEARTBEAT_HOURS=20
STALE_HOURS=48

DS_BASE="https://api.pollis.com"
LOG_BASE="https://verify.pollis.com"
CDN_BASE="https://cdn.pollis.com"
SITE_BASE="https://pollis.com"

for tool in curl jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required" >&2
    exit 127
  fi
done

now_iso() { date -u +%Y-%m-%dT%H:%M:%SZ; }

# Seconds between two RFC 3339 timestamps, using whichever `date` this is.
epoch_of() {
  local ts="$1"
  date -u -d "$ts" +%s 2>/dev/null || date -u -j -f "%Y-%m-%dT%H:%M:%SZ" "$ts" +%s 2>/dev/null || echo 0
}

# ── --check: is the record itself alive? ──────────────────────────────────────
# Runs in website-verify.yml, so a prober that has stopped pushing is caught
# even when the push is what broke. Deliberately independent of the probe path.
if [ "${1:-}" = "--check" ]; then
  if [ ! -f "$OUT" ]; then
    echo "::error::$OUT does not exist — /status has nothing to render."
    exit 1
  fi
  newest="$(jq -r '.samples[-1].at // ""' "$OUT")"
  if [ -z "$newest" ]; then
    echo "::error::$OUT holds no samples — the prober has never recorded one."
    exit 1
  fi
  age_h=$(( ( $(date -u +%s) - $(epoch_of "$newest") ) / 3600 ))
  if [ "$age_h" -gt "$STALE_HOURS" ]; then
    echo "::error::the newest status sample is ${age_h}h old (limit ${STALE_HOURS}h)."
    echo "::error::  status-probe.yml has stopped recording, so /status is showing a"
    echo "::error::  stale picture while looking authoritative. Check that the workflow"
    echo "::error::  still runs (GitHub disables schedules on inactive repos) and that"
    echo "::error::  its push to main is not being rejected."
    exit 1
  fi
  echo "✓ newest status sample is ${age_h}h old (limit ${STALE_HOURS}h)"
  exit 0
fi

# ── probe ─────────────────────────────────────────────────────────────────────
# One request per target. `-o` keeps the body (some targets are read for a
# field), `-w` gives the status and the wall time. A non-2xx is recorded, not
# fatal: recording a failure is the entire point.
probe() {
  local url="$1" body_file="$2"
  local out
  out="$(curl -sS -m 20 -o "$body_file" -w '%{http_code} %{time_total}' "$url" 2>/dev/null || true)"
  if [ -z "$out" ]; then
    echo "000 0"
    return
  fi
  echo "$out"
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

read -r ds_health_code ds_health_t <<<"$(probe "${DS_BASE}/health" "${tmp}/ds_health")"
read -r ds_ver_code ds_ver_t <<<"$(probe "${DS_BASE}/version" "${tmp}/ds_version")"
read -r log_code log_t <<<"$(probe "${LOG_BASE}/v1/sth/latest.json" "${tmp}/log")"
read -r acct_code _acct_t <<<"$(probe "${LOG_BASE}/v1/account-keys/sth/latest.json" "${tmp}/acct")"
read -r bin_code _bin_t <<<"$(probe "${LOG_BASE}/v1/binaries/sth/latest.json" "${tmp}/bin")"
read -r cdn_code cdn_t <<<"$(probe "${CDN_BASE}/releases/latest.json" "${tmp}/cdn")"
read -r site_code site_t <<<"$(probe "${SITE_BASE}/" "${tmp}/site")"

json_field() {
  local file="$1" filter="$2"
  jq -r "$filter // empty" "$file" 2>/dev/null || true
}

ds_sha="$(json_field "${tmp}/ds_version" '.sha')"
release="$(json_field "${tmp}/cdn" '.version')"
commit_size="$(json_field "${tmp}/log" '.tree_size')"
account_size="$(json_field "${tmp}/acct" '.tree_size')"
binaries_size="$(json_field "${tmp}/bin" '.tree_size')"

# The transparency PUBLISHER, separately from the tree it serves. These are not
# the same signal and conflating them is the failure mode #877 called out: an
# STH's timestamp is FROZEN per tree size (transparency-publish.yml reuses the
# published head's timestamp unless the tree grew), so an old timestamp is the
# expected look of an idle log AND the look of a log nobody is publishing any
# more. Only the publisher's own run history separates them. Public API, so any
# third party can check the same thing without our credentials.
pub_at=""; pub_conclusion="unknown"; pub_url=""
if command -v gh >/dev/null 2>&1; then
  pub_json="$(gh run list --workflow=transparency-publish.yml --limit 1 \
    --json conclusion,createdAt,url 2>/dev/null || echo '[]')"
  if [ "$(jq 'length' <<<"$pub_json" 2>/dev/null || echo 0)" -gt 0 ]; then
    pub_at="$(jq -r '.[0].createdAt' <<<"$pub_json")"
    pub_conclusion="$(jq -r '.[0].conclusion // "in_progress"' <<<"$pub_json")"
    pub_url="$(jq -r '.[0].url' <<<"$pub_json")"
  fi
fi

mk_target() {
  jq -nc --arg code "$1" --arg t "$2" \
    '{ok: ($code | startswith("2")), status: ($code | tonumber), ms: (($t | tonumber) * 1000 | round)}'
}

sample="$(jq -nc \
  --arg at "$(now_iso)" \
  --argjson ds "$(mk_target "$ds_health_code" "$ds_health_t")" \
  --argjson ds_version "$(mk_target "$ds_ver_code" "$ds_ver_t")" \
  --argjson log "$(mk_target "$log_code" "$log_t")" \
  --argjson cdn "$(mk_target "$cdn_code" "$cdn_t")" \
  --argjson site "$(mk_target "$site_code" "$site_t")" \
  --arg ds_sha "${ds_sha:-}" \
  --arg release "${release:-}" \
  --arg commit_size "${commit_size:-}" \
  --arg account_size "${account_size:-}" \
  --arg binaries_size "${binaries_size:-}" \
  --arg acct_code "$acct_code" \
  --arg bin_code "$bin_code" \
  --arg pub_at "$pub_at" \
  --arg pub_conclusion "$pub_conclusion" \
  --arg pub_url "$pub_url" \
  '{
     at: $at,
     targets: {ds: $ds, ds_version: $ds_version, log: $log, cdn: $cdn, site: $site},
     ds_sha: (if $ds_sha == "" then null else $ds_sha end),
     latest_release: (if $release == "" then null else $release end),
     trees: {
       commit_log: (if $commit_size == "" then null else ($commit_size | tonumber) end),
       account_keys: (if $account_size == "" then null else ($account_size | tonumber) end),
       binaries: (if $binaries_size == "" then null else ($binaries_size | tonumber) end),
       account_keys_ok: ($acct_code | startswith("2")),
       binaries_ok: ($bin_code | startswith("2"))
     },
     publisher: {
       last_run_at: (if $pub_at == "" then null else $pub_at end),
       conclusion: $pub_conclusion,
       run_url: (if $pub_url == "" then null else $pub_url end)
     }
   }')"

# ── append policy ─────────────────────────────────────────────────────────────
# Compared on the fields that MEAN something changed. Response times are
# recorded but deliberately excluded: they differ on every run, so including
# them would make every sample a "change" and drown the record in noise.
comparable() {
  jq -Sc '{
    targets: (.targets | map_values({ok, status})),
    ds_sha, latest_release,
    trees: (.trees | {commit_log, account_keys, binaries, account_keys_ok, binaries_ok}),
    publisher: (.publisher | {conclusion})
  }' <<<"$1"
}

if [ ! -f "$OUT" ]; then
  echo "error: $OUT is missing — it is a committed file, not a generated one." >&2
  exit 1
fi

prev="$(jq -c '.samples[-1] // null' "$OUT")"
reason="change"
if [ "$prev" != "null" ]; then
  if [ "$(comparable "$sample")" = "$(comparable "$prev")" ]; then
    prev_at="$(jq -r '.at' <<<"$prev")"
    age_h=$(( ( $(date -u +%s) - $(epoch_of "$prev_at") ) / 3600 ))
    if [ "$age_h" -lt "$HEARTBEAT_HOURS" ]; then
      echo "no change since ${prev_at} (${age_h}h ago, heartbeat every ${HEARTBEAT_HOURS}h) — nothing recorded"
      exit 0
    fi
    reason="heartbeat"
  fi
else
  reason="first"
fi

updated="$(jq \
  --argjson sample "$sample" \
  --arg reason "$reason" \
  --arg generated "$(now_iso)" \
  --argjson heartbeat "$HEARTBEAT_HOURS" \
  '.generated_at = $generated
   | .heartbeat_hours = $heartbeat
   | .samples += [$sample + {reason: $reason}]' "$OUT")"

printf '%s\n' "$updated" | jq . >"${OUT}.tmp"
mv "${OUT}.tmp" "$OUT"
echo "recorded a '${reason}' sample at $(jq -r '.samples[-1].at' "$OUT")"
