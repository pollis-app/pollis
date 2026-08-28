#!/usr/bin/env bash
# Self-test for scripts/check-incidents.py (#877).
#
# The incident record is the one page a user reads to find out whether something
# went wrong, so its validator has to be a gate rather than a decoration. Each
# case below is a way the record has to be able to go red — a missed postmortem
# deadline most of all, because that is the failure that turns a status page
# into a marketing page one quiet omission at a time.
#
# Offline, no network, no deps beyond python3.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHECK="${REPO_ROOT}/scripts/check-incidents.py"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

pass=0
fail=0

# expect_fail <name> <json>
expect_fail() {
  local name="$1" body="$2" file="${tmp}/case.json"
  printf '%s' "$body" >"$file"
  if python3 "$CHECK" "$file" >/dev/null 2>&1; then
    echo "  ✗ $name — check-incidents.py PASSED a record it must reject"
    fail=$((fail + 1))
  else
    echo "  ✓ $name — rejected"
    pass=$((pass + 1))
  fi
}

expect_pass() {
  local name="$1" body="$2" file="${tmp}/case.json"
  printf '%s' "$body" >"$file"
  if python3 "$CHECK" "$file" >/dev/null 2>&1; then
    echo "  ✓ $name — accepted"
    pass=$((pass + 1))
  else
    echo "  ✗ $name — check-incidents.py REJECTED a valid record"
    python3 "$CHECK" "$file" || true
    fail=$((fail + 1))
  fi
}

# A resolved sev3 with no correctness impact owes no postmortem, however old.
expect_pass "valid record, sev3 closed without a postmortem" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-cdn-slow", "title": "Downloads were slow", "severity": "sev3",
    "status": "resolved", "correctness_impact": false, "components": ["cdn"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": "2026-01-02T11:00:00Z",
    "user_impact": "Downloads took longer than usual.", "summary": "Edge cache miss storm.",
    "postmortem": null
  }]
}'

# The one that matters: a sev2 resolved long ago with no postmortem.
expect_fail "sev2 resolved 60 days ago with no postmortem" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-ds-down", "title": "DS unreachable", "severity": "sev2",
    "status": "resolved", "correctness_impact": false, "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": "2026-01-02T11:00:00Z",
    "user_impact": "Nobody could send or receive.", "summary": "Container failed to boot.",
    "postmortem": null
  }]
}'

# Same, driven by the correctness flag rather than the severity.
expect_fail "sev3 with correctness_impact and no postmortem" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-watermark", "title": "Watermarks not recorded", "severity": "sev3",
    "status": "resolved", "correctness_impact": true, "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": "2026-01-02T11:00:00Z",
    "user_impact": "Devices could not report a delivery cursor.", "summary": "Write path rejected reports.",
    "postmortem": null
  }]
}'

expect_fail "correctness_impact omitted" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-ds-down", "title": "DS unreachable", "severity": "sev3",
    "status": "resolved", "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": "2026-01-02T11:00:00Z",
    "user_impact": "Slow.", "summary": "Boot.", "postmortem": null
  }]
}'

expect_fail "resolved status with a null resolved_at" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-ds-down", "title": "DS unreachable", "severity": "sev4",
    "status": "resolved", "correctness_impact": false, "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": null,
    "user_impact": "None.", "summary": "Nothing.", "postmortem": null
  }]
}'

expect_fail "component that is not a probed target" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-mystery", "title": "Something", "severity": "sev4",
    "status": "investigating", "correctness_impact": false, "components": ["not-a-service"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": null,
    "user_impact": "Unknown.", "summary": "Unknown.", "postmortem": null
  }]
}'

expect_fail "postmortem path that does not exist" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-ds-down", "title": "DS unreachable", "severity": "sev2",
    "status": "resolved", "correctness_impact": false, "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": "2026-01-02T11:00:00Z",
    "user_impact": "Nobody could send.", "summary": "Boot failure.",
    "postmortem": "docs/incidents/2026-01-02-ds-down.md"
  }]
}'

expect_fail "empty user_impact" '{
  "record_started_at": "2026-08-28",
  "incidents": [{
    "id": "2026-01-02-ds-down", "title": "DS unreachable", "severity": "sev4",
    "status": "investigating", "correctness_impact": false, "components": ["ds"],
    "started_at": "2026-01-02T10:00:00Z", "resolved_at": null,
    "user_impact": "  ", "summary": "Boot failure.", "postmortem": null
  }]
}'

expect_fail "record_started_at missing" '{
  "incidents": []
}'

expect_fail "malformed JSON" '{ "incidents": [ '

echo
echo "check-incidents self-test: ${pass} passed, ${fail} failed"
if [ "$fail" -ne 0 ]; then
  exit 1
fi
