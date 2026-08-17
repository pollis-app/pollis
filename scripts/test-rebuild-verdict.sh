#!/usr/bin/env bash
#
# test-rebuild-verdict.sh — tests for the independent rebuilder's verdict logic
# (#944).
#
# The bug this exists to prevent is not "the rebuilder went red". It is "the
# rebuilder went red and the red meant something other than what it said". A
# reproducibility signal is only worth publishing if a reader can act on it, and
# a reader can only act on it if each distinct cause reads differently.
#
# So the assertions come in matched pairs, and the important half of every pair
# is the one that must STILL FAIL. It is easy to make an inconvenient red go
# away; a change that did that to the genuine `did_not_reproduce` case would be
# strictly worse than the ambiguity it replaced, because it would silence the
# one finding the whole binary-transparency programme is built to surface.
#
# Run: ./scripts/test-rebuild-verdict.sh   (no arguments, no network)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib/rebuild-verdict.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); echo "  ok   — $1"; }
bad()  { fail=$((fail+1)); echo "  FAIL — $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

if ! command -v jq >/dev/null 2>&1; then
  echo "error: jq is required" >&2
  exit 127
fi

# The two real images from the v1.9.3 incident, used verbatim so the test names
# the case it was written for.
REL_IMAGE="ubuntu22@20260810.260.1+helper:ubuntu24@20260810.260.1"
REB_IMAGE="ubuntu22@20260720.234.2+helper:ubuntu24@20260720.234.2"
LOGGED="6c1f0f1ad0dbb0f37e1b0e1a4b4a2d0c6f2a9c5e8d7b3a1f0e9d8c7b6a5f4e3d"
OTHER="a6767a4cef545eb1aba628eb535f98b61d296798deeffa4c1d332f98fcd89731"

echo "rebuild_classify — the four outcomes"

# THE DIRECTION THAT MUST KEEP FAILING. Same recorded image, different bytes:
# nothing explains the divergence, so it is the accusation, and it stays one.
check "same image + differing bytes is a real non-reproduction" \
  "$(rebuild_classify "$OTHER" "$LOGGED" "$REL_IMAGE" "$REL_IMAGE")" "did_not_reproduce"
if rebuild_verdict_is_accusation "$(rebuild_classify "$OTHER" "$LOGGED" "$REL_IMAGE" "$REL_IMAGE")"; then
  ok "…and it is flagged as an accusation"
else
  bad "…and it is flagged as an accusation"
fi

# v1.9.3 itself.
check "different image + differing bytes is environment drift" \
  "$(rebuild_classify "$OTHER" "$LOGGED" "$REL_IMAGE" "$REB_IMAGE")" "environment_drift"
if rebuild_verdict_is_accusation "$(rebuild_classify "$OTHER" "$LOGGED" "$REL_IMAGE" "$REB_IMAGE")"; then
  bad "…and it does NOT accuse the shipped binary"
else
  ok "…and it does NOT accuse the shipped binary"
fi

# A helper-image-only difference is still a different environment: the AppImage
# embeds a binary compiled in that job, so it may not be waved through.
check "matching app image but drifted helper image is still drift" \
  "$(rebuild_classify "$OTHER" "$LOGGED" \
       "ubuntu22@20260810.260.1+helper:ubuntu24@20260810.260.1" \
       "ubuntu22@20260810.260.1+helper:ubuntu24@20260720.234.2")" "environment_drift"

check "unrecorded recipe + differing bytes is inconclusive, not a finding" \
  "$(rebuild_classify "$OTHER" "$LOGGED" "" "$REB_IMAGE")" "recipe_unrecorded"
check "unidentifiable local runner is equally inconclusive" \
  "$(rebuild_classify "$OTHER" "$LOGGED" "$REL_IMAGE" "")" "recipe_unrecorded"

echo "rebuild_classify — a match is a match"

check "identical hashes on the identical image reproduce" \
  "$(rebuild_classify "$LOGGED" "$LOGGED" "$REL_IMAGE" "$REL_IMAGE")" "reproduced"
# Reproducing DESPITE a drifted environment is a stronger result than
# reproducing on the same one, so the environment check must never be allowed to
# downgrade a genuine byte-for-byte match.
check "identical hashes on a drifted image still reproduce" \
  "$(rebuild_classify "$LOGGED" "$LOGGED" "$REL_IMAGE" "$REB_IMAGE")" "reproduced"
check "identical hashes with no recorded recipe still reproduce" \
  "$(rebuild_classify "$LOGGED" "$LOGGED" "" "$REB_IMAGE")" "reproduced"

# An empty reproduced hash means the rebuild produced nothing to compare. It
# must never satisfy the equality test against an equally empty logged hash and
# report success — that is the "vacuously true over an empty set" failure #939
# fixed one layer up, and it does not get to reappear here.
check "an empty rebuild hash never counts as reproduced" \
  "$(rebuild_classify "" "" "$REL_IMAGE" "$REL_IMAGE")" "did_not_reproduce"

echo "report readers — against report shapes the log actually serves"

# Post-#939 shape: the leaf carries a real recipe.
cat > "$work/recorded.json" <<JSON
{"release_tag":"v1.9.9","found":true,"artifacts":[
  {"platform":"darwin","bundle":"dmg","layer":"payload","payload_sha256":"dead",
   "toolchain":{"rustc":"1.96.0","node":"26.4.0","pnpm":"10.26.1","runner_image":"macos14@1","source_date_epoch":1}},
  {"platform":"linux","bundle":"appimage","layer":"payload","payload_sha256":"${LOGGED}",
   "toolchain":{"rustc":"1.96.0","node":"26.4.0","pnpm":"10.26.1","runner_image":"${REL_IMAGE}","source_date_epoch":1}},
  {"platform":"linux","bundle":"deb","layer":"payload","payload_sha256":"beef",
   "toolchain":{"rustc":"1.96.0","node":"26.4.0","pnpm":"10.26.1","runner_image":"${REL_IMAGE}","source_date_epoch":1}}
]}
JSON
check "reads the AppImage payload hash, not the .deb's or macOS's" \
  "$(rebuild_logged_linux_payload "$work/recorded.json")" "$LOGGED"
check "reads the AppImage leaf's runner image" \
  "$(rebuild_logged_runner_image "$work/recorded.json")" "$REL_IMAGE"

# Pre-#939 shape: every field is the literal string "unknown". This is the shape
# for EVERY tag through v1.9.7, so getting it wrong would misclassify the entire
# published history — and treating "unknown" as an image would make it compare
# unequal to ours and silently relabel every old red as environment drift.
cat > "$work/unknown.json" <<JSON
{"release_tag":"v1.9.3","found":true,"artifacts":[
  {"platform":"linux","bundle":"appimage","layer":"payload","payload_sha256":"${LOGGED}",
   "toolchain":{"rustc":"unknown","node":"unknown","pnpm":"unknown","runner_image":"unknown","source_date_epoch":0}}
]}
JSON
check "an \"unknown\" runner image reads as unrecorded, not as an image" \
  "$(rebuild_logged_runner_image "$work/unknown.json")" ""

# Pre-toolchain shape: the field is absent entirely (a report generated by a
# verifier build older than this change).
cat > "$work/absent.json" <<JSON
{"release_tag":"v1.8.4","found":true,"artifacts":[
  {"platform":"linux","bundle":"appimage","layer":"payload","payload_sha256":"${LOGGED}"}
]}
JSON
check "an absent toolchain object reads as unrecorded" \
  "$(rebuild_logged_runner_image "$work/absent.json")" ""
check "…and its payload hash is still readable" \
  "$(rebuild_logged_linux_payload "$work/absent.json")" "$LOGGED"

# A tag with no Linux AppImage leaf at all: the readers must yield empty rather
# than jq-null, so the classifier's empty-string guards actually fire.
cat > "$work/nolinux.json" <<JSON
{"release_tag":"v1.0.0","found":true,"artifacts":[
  {"platform":"darwin","bundle":"dmg","layer":"payload","payload_sha256":"dead"}
]}
JSON
check "no Linux AppImage leaf yields an empty payload hash" \
  "$(rebuild_logged_linux_payload "$work/nolinux.json")" ""
check "no Linux AppImage leaf yields an empty runner image" \
  "$(rebuild_logged_runner_image "$work/nolinux.json")" ""

echo "wiring — the rebuilder must not source its own tooling from the tag"

# The bug this catches, found live on the first end-to-end run of #944's change:
# every checkout in rebuild-verify.yml is `ref: $TAG`, so `scripts/` in the
# workspace is the RELEASED tag's copy. Sourcing this library from there works on
# a tag cut after it landed and fails on every older one — and it failed
# SILENTLY, because `$(runner_image_id)` on a missing function yields an empty
# string that `echo` happily accepts. The job stayed green having recorded
# nothing, which would have degraded every later verdict to "inconclusive" while
# looking fine.
#
# The rule: anything whose CURRENT behaviour the verdict depends on is sourced
# from `.rebuild-tools` (a second checkout at the workflow's own ref).
# `payload-hash.sh` is the deliberate exception — that one must be the tag's, so
# the reproducer hashes the payload exactly as the release did.
wf="$(cd "$here/.." && pwd)/.github/workflows/rebuild-verify.yml"
if [ ! -f "$wf" ]; then
  bad "rebuild-verify.yml not found at $wf"
else
  for lib in rebuild-verdict.sh attest-helpers.sh; do
    if grep -qE "^\s*\.\s+scripts/lib/${lib}" "$wf"; then
      bad "$lib is sourced from the workspace (the tag's copy), not .rebuild-tools"
    else
      ok "$lib is not sourced from the tag's workspace copy"
    fi
  done
  if grep -qE "^\s*\.\s+\.rebuild-tools/scripts/lib/rebuild-verdict\.sh" "$wf"; then
    ok "the verdict library is sourced from the workflow's own ref"
  else
    bad "the verdict library is not sourced from .rebuild-tools"
  fi
  # The exception, asserted so nobody "fixes" it into consistency.
  if grep -qE "^\s*\.\s+scripts/lib/payload-hash\.sh" "$wf"; then
    ok "payload-hash.sh still comes from the tag, as it must"
  else
    bad "payload-hash.sh must be sourced from the tag's workspace, not .rebuild-tools"
  fi
fi

echo
echo "rebuild verdict: ${pass} passed, ${fail} failed"
[ "$fail" -eq 0 ]
