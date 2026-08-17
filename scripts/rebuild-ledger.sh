#!/usr/bin/env bash
# Regenerate website/rebuild-ledger.json — the durable public record of every
# release's independent-rebuild verdict (#877).
#
# Why this exists. `rebuild-verify.yml` genuinely rebuilds the Linux AppImage
# payload for a released tag from public source, with no Pollis secrets, and
# asserts the hash it gets against the public ML-DSA-44 binaries transparency
# log. That is the strongest reproducibility claim the project has — and until
# now its verdict went to a job log and a default-90-day CI artifact, with no
# page linking to it. After 90 days the evidence for "every release is rebuilt"
# was simply gone. This turns each verdict into a committed file the website
# renders and git history retains permanently.
#
# Why a script rather than a step inside rebuild-verify.yml. That workflow is
# deliberately a SEPARATE TRUST DOMAIN: it references no `secrets.*`, so a third
# party can fork it and run the identical check with no credentials from us (see
# its header). Giving it credentials to publish its own verdict would destroy
# the one property that makes its verdict worth reading. So the verdict is
# collected out-of-band, from the public Actions API, by this script.
#
# The ledger cannot silently rot: `website-verify.yml` runs `--check` daily and
# fails when new runs are unpublished.
#
# TAG RESOLUTION, and why it is layered. A `workflow_run`-triggered run reports
# `headBranch: main` and a `headSha` that is main's tip, NOT the tag's commit —
# the tag lives only in `github.event.workflow_run.head_branch`, which the API
# does not expose on the triggered run. So:
#   1. `run-name:` in rebuild-verify.yml now puts the tag in the run title, so
#      every FUTURE run is resolved from `displayTitle` with no extra calls.
#   2. Older runs are resolved by grepping their job log, which is authoritative.
#   3. Logs expire (90 days), so a tag already recorded in the committed ledger
#      is reused and never re-fetched. The file is its own permanent memory —
#      which is the whole point of the exercise.
#
# Usage:
#   scripts/rebuild-ledger.sh              # rewrite website/rebuild-ledger.json
#   scripts/rebuild-ledger.sh --check      # exit 1 if it is out of date
#
# Requires: gh (authenticated), jq.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${REPO_ROOT}/website/rebuild-ledger.json"
WORKFLOW="rebuild-verify.yml"

# The commit that fixed the false-red bug (#939). Before it, this workflow could
# report "did not reproduce" for a release that was merely not yet published
# into the log: `pollis-verify release` exits 0 over an empty artifact set, so
# the wait loop broke on the first attempt and the rebuild was compared against
# nothing. Two release runs went red exactly that way (v1.9.4, v1.9.5) and both
# in fact reproduced byte-identically. The ledger carries the boundary so a
# reader is not left to assume every historical red was a real reproducibility
# failure — and so that nobody quietly deletes the reds instead.
FALSE_RED_FIX_COMMIT="564353ae"
FALSE_RED_FIX_DATE="2026-08-16"

for tool in gh jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "error: $tool is required" >&2
    exit 127
  fi
done

# The workflow has run ~30 times in total, so 100 covers its entire history in
# one API page and keeps this cheap enough to run on a daily cron.
runs="$(gh run list --workflow="$WORKFLOW" --limit 100 \
  --json conclusion,createdAt,event,databaseId,displayTitle,headSha,headBranch 2>/dev/null)"

if [ -z "$runs" ] || [ "$runs" = "[]" ]; then
  echo "error: no runs returned for $WORKFLOW — is gh authenticated?" >&2
  exit 1
fi

prev='{"runs":[]}'
if [ -f "$OUT" ]; then
  prev="$(cat "$OUT")"
fi

resolved="[]"
while IFS=$'\t' read -r run_id created_at conclusion event head_sha head_branch title; do
  [ -n "$run_id" ] || continue

  # 0. Only runs of the workflow AS SHIPPED count as verdicts about a release.
  #
  #    A run on a feature branch is the workflow being BUILT, not a judgement on
  #    a release — but when it was dispatched against a real tag (which is how
  #    you test a reproducer at all) the tag resolves and it used to be published
  #    as that release's verdict. Fourteen such runs existed, thirteen of them
  #    red: nine from `fix/build-recipe-drift`, three from `diag/rebuild-artifact`
  #    and two from `feature/auto-verify-releases`, every one of them a red
  #    belonging to a workflow that was still being debugged, all of them
  #    attributed to v1.8.4. The existing "no tag means a development run"
  #    exclusion below only catches the ones where nothing resolved, which is the
  #    weaker half of the same rule.
  #
  #    Tag-triggered runs report the tag as the branch (`v1.9.3`), and
  #    `workflow_run`/`workflow_dispatch` runs from the default branch report
  #    `main`, so those two shapes are the whole of the legitimate set.
  case "$head_branch" in
    main|v[0-9]*) ;;
    *) continue ;;
  esac

  # 1. Already in the committed ledger — trust it and never re-fetch. This is
  #    what keeps verdicts readable long after their logs have expired.
  tag="$(printf '%s' "$prev" | jq -r --arg id "$run_id" \
    '(.runs // []) | map(select(.run_id == $id)) | .[0].tag // ""')"

  # 2. The run title (populated by `run-name:` from #877 onward).
  if [ -z "$tag" ]; then
    tag="$(printf '%s' "$title" | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' | head -1 || true)"
  fi

  # 3. The job log, while it still exists.
  if [ -z "$tag" ] && [ "$conclusion" != "skipped" ]; then
    tag="$(gh run view "$run_id" --log 2>/dev/null \
      | grep -oE 'tag v[0-9]+\.[0-9]+\.[0-9]+' | head -1 \
      | grep -oE 'v[0-9]+\.[0-9]+\.[0-9]+' || true)"
  fi

  # 4. `headSha` -> tag, via git. This is the ONLY resolution that works for a
  #    SKIPPED run, which has no log at all — and skipped runs are the ones that
  #    matter most to record honestly, because a skipped run means a release was
  #    never rebuild-verified even though the workflow "ran after" it. Leaving
  #    them out would turn "runs after every release" into an unearned claim.
  #    Unreliable in general (headSha is main's tip for a workflow_run, which
  #    coincides with the tag commit only when the release tagged main's tip),
  #    hence last.
  if [ -z "$tag" ]; then
    tag="$(git -C "$REPO_ROOT" tag --points-at "$head_sha" 2>/dev/null \
      | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | head -1 || true)"
  fi

  # No tag means this was a development run on a feature branch — the workflow
  # being built, not a verdict about a release. Publishing those as failures
  # would badly misrepresent the record, so they are excluded.
  [ -n "$tag" ] || continue

  resolved="$(printf '%s' "$resolved" | jq \
    --arg tag "$tag" \
    --arg conclusion "$conclusion" \
    --arg created_at "$created_at" \
    --arg event "$event" \
    --arg run_id "$run_id" \
    '. += [{
       tag: $tag,
       conclusion: $conclusion,
       created_at: $created_at,
       event: $event,
       run_id: $run_id,
       run_url: ("https://github.com/actuallydan/pollis/actions/runs/" + $run_id)
     }]')"
done < <(printf '%s' "$runs" | jq -r '.[] | [
  (.databaseId|tostring), .createdAt, .conclusion, .event, .headSha,
  (.headBranch // ""), (.displayTitle // "")
] | @tsv')

# Per-run classification of the pre-fix reds, established by reading the run
# logs rather than inferred from the date. "Before the fix" is necessary but not
# sufficient to call a red false — v1.9.3 was red before the fix AND genuinely
# produced different bytes. Publishing a blanket "reds before this date may be
# false" would quietly launder a real finding, so each one is classified
# individually and the evidence is recorded next to it.
#
# Three classes, because two were not enough (#944):
#   false_red          the run never performed the comparison (the #939 wait-loop
#                      bug); the build in fact reproduced byte-identically.
#   environment_drift  the comparison was performed and the bytes differed, but
#                      the rebuild did not run in the environment the release
#                      recorded, so the divergence is not attributable to the
#                      source. Still red — inconclusive is not a pass.
#   real               the bytes differed with the environment held constant.
#                      The accusation. No run currently carries this class, and
#                      that fact should be checked, not assumed.
CLASSIFIED='{
  "31753766171": {
    "class": "environment_drift",
    "evidence": "Explained by #944, and not a finding against the shipped binary. The tag was published and the log verified (found: true), and the AppImage genuinely differed: logged 07bccca8…, rebuilt a6767a4c…. The cause is that the two builds ran on different CI machine images — the release on ubuntu22@20260810.260.1, the rebuild 36 minutes later on ubuntu22@20260720.234.2, because GitHub rolls a re-imaged ubuntu-22.04 label out gradually. (Both read from the two runs'"'"' own logs; this tag'"'"'s leaf predates #939 and records runner_image \"unknown\", which is why the workflow could not tell at the time.) An AppImage vendors the app'"'"'s system libraries off the build machine, and seven differed in real code — libgssapi_krb5, libk5crypto, libkrb5, libkrb5support, libsqlite3, libsystemd, libudev — with changed .text sizes and shifted symbol addresses, not merely new build ids. The 105,863,272 differing bytes are compression smear across a squashfs, not the size of the change. The only difference in anything built from Pollis source was the ORDER of the twelve CSP content hashes inside usr/bin/pollis: the same twelve values, permuted, because tauri-codegen emits them in filesystem readdir order. That the twelve digests are identical is itself proof the frontend bundle reproduced byte-for-byte. Nothing indicates the shipped binary differs from the tagged source. Proven, not merely argued: re-running the identical rebuild on 2026-08-17 landed on ubuntu22@20260810.260.1 — the exact image the release built on — and reproduced 07bccca8… byte-for-byte (run 32036282718). Same source, same tag, one variable changed, and the bytes match. Full write-up: docs/reproducible-builds-residuals.md §6 and §6a."
  },
  "31866040326": {
    "class": "false_red",
    "evidence": "The tag was not yet in the binaries tree (found: false) and 0 bytes differed. v1.9.4 reproduced byte-identically; the red was the wait-loop bug."
  },
  "31903478733": {
    "class": "false_red",
    "evidence": "The tag was not yet in the binaries tree (found: false) and 0 bytes differed. v1.9.5 reproduced byte-identically; the red was the wait-loop bug."
  }
}'

jq -n \
  --arg generated "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg fix_commit "$FALSE_RED_FIX_COMMIT" \
  --arg fix_date "$FALSE_RED_FIX_DATE" \
  --argjson classified "$CLASSIFIED" \
  --argjson resolved "$resolved" '
  {
    generated_at: $generated,
    workflow: "rebuild-verify.yml",
    false_red_fix: { commit: $fix_commit, date: $fix_date },
    runs: (
      $resolved
      | map(. + (($classified[.run_id] // {}) | {classification: .class, evidence: .evidence}))
      | sort_by(.created_at) | reverse
    )
  }' > "${OUT}.tmp"

if [ "${1:-}" = "--check" ]; then
  if [ ! -f "$OUT" ]; then
    echo "::error::website/rebuild-ledger.json does not exist. Run scripts/rebuild-ledger.sh" >&2
    rm -f "${OUT}.tmp"
    exit 1
  fi
  # Compare on content only. Including the regenerated timestamp would fail the
  # check on every run for no reason, and a check that always fails gets muted.
  a="$(jq -S 'del(.generated_at)' "$OUT")"
  b="$(jq -S 'del(.generated_at)' "${OUT}.tmp")"
  rm -f "${OUT}.tmp"
  if [ "$a" != "$b" ]; then
    echo "::error::website/rebuild-ledger.json is out of date — rebuild-verify has run" >&2
    echo "::error::  since it was last published. Run scripts/rebuild-ledger.sh and commit" >&2
    echo "::error::  the result, so /assurance stops under-reporting the record." >&2
    exit 1
  fi
  echo "rebuild ledger is up to date."
  exit 0
fi

mv "${OUT}.tmp" "$OUT"
echo "wrote $OUT ($(jq '.runs | length' "$OUT") release runs)"
