#!/usr/bin/env bash
#
# Triage one finished, not-green scheduled run of mls-tests.yml (#1803).
#
# Run by .github/workflows/mls-nightly-triage.yml after the run completes. Two
# outcomes, never both:
#
#   1. A job never got a runner. GitHub drops a hosted job it cannot place within
#      15 minutes ("The job was not acquired by Runner of type hosted even after
#      multiple attempts"): no runner name, no steps. That is what turned the
#      2026-11-04 and 2026-11-10 nightlies red, both times `macos-at-rest` during a
#      macOS arm64 capacity squeeze, with every test that did run passing. The run
#      is re-run (failed and cancelled jobs plus `gate`), up to MAX_ATTEMPTS, and
#      nothing is filed. The re-run's own completion triggers this script again.
#   2. Anything else (a failing test, a timeout, a cancellation, or a runner still
#      missing after the last attempt): one tracking issue, opened if none is open
#      and commented on otherwise, so a red night always reaches the owner and a
#      run of red nights stays in one thread. The body names each non-green job
#      with its own failure annotation, so the cause is readable without opening
#      the logs.
#
# Usage: RUN_ID=<id> GH_REPO=<owner/repo> scripts/ci-nightly-triage.sh
#   RUN_ATTEMPT    the attempt to judge (default: the run's latest)
#   MAX_ATTEMPTS   re-run limit for runner loss (default 3)
#   NIGHTLY_OWNER  assignee of a new issue (default actuallydan)
#   DRY_RUN=1      print what would be done, change nothing
set -euo pipefail

: "${RUN_ID:?RUN_ID is required}"
: "${GH_REPO:?GH_REPO is required}"
MAX_ATTEMPTS="${MAX_ATTEMPTS:-3}"
# The repo belongs to an org, which cannot be assigned; a person can.
NIGHTLY_OWNER="${NIGHTLY_OWNER:-actuallydan}"
DRY_RUN="${DRY_RUN:-}"
TITLE="Nightly MLS tests failing"

run="$(gh api "repos/$GH_REPO/actions/runs/$RUN_ID")"
attempt="${RUN_ATTEMPT:-$(jq -r '.run_attempt' <<<"$run")}"
conclusion="$(jq -r '.conclusion' <<<"$run")"
sha="$(jq -r '.head_sha' <<<"$run")"
url="$(jq -r '.html_url' <<<"$run")"

jobs="$(gh api "repos/$GH_REPO/actions/runs/$RUN_ID/attempts/$attempt/jobs?per_page=100")"

echo "run $RUN_ID attempt $attempt: $conclusion"
jq -r '.jobs[] | "  \(.name): \(.conclusion)"' <<<"$jobs"

if [ "$conclusion" = "success" ]; then
  echo "Green; nothing to do."
  exit 0
fi

# Jobs that ended without ever starting on a runner.
unplaced="$(jq -r '.jobs[]
  | select(.conclusion != "success" and .conclusion != "skipped")
  | select((.runner_name // "") == "" and ((.steps // []) | length) == 0)
  | .name' <<<"$jobs")"

# Only when the run FAILED: `gate` ran and went red because of the unplaced job.
# A run cancelled outright (by a person) also leaves never-started jobs, and
# re-running it would undo the cancellation.
if [ -n "$unplaced" ] && [ "$conclusion" = "failure" ] && [ "$attempt" -lt "$MAX_ATTEMPTS" ]; then
  echo "Never got a runner: $(tr '\n' ' ' <<<"$unplaced")— re-running (attempt $((attempt + 1)) of $MAX_ATTEMPTS)."
  if [ -z "$DRY_RUN" ]; then
    gh run rerun "$RUN_ID" --failed
  fi
  exit 0
fi

# One line per job that did not pass, with the failure annotation GitHub
# recorded for it (timeouts, runner loss and `::error::` lines all land there),
# preferring a specific one over the generic "Process completed with exit code".
details=""
while IFS=$'\t' read -r id name result; do
  [ -n "$id" ] || continue
  why="$(gh api "repos/$GH_REPO/check-runs/$id/annotations" \
    --jq '[.[] | select(.annotation_level == "failure") | .message]
      | (map(select(startswith("Process completed") | not))[0] // .[0] // "")' 2>/dev/null || true)"
  details+="- \`$name\`: $result${why:+ — $why}"$'\n'
done < <(jq -r '.jobs[]
  | select(.conclusion != "success" and .conclusion != "skipped")
  | [.id, .name, .conclusion] | @tsv' <<<"$jobs")

if [ -n "$unplaced" ] && [ "$conclusion" = "failure" ]; then
  details+=$'\n'"A job still had no runner after $attempt attempts, so this is runner capacity, not a test result."$'\n'
fi

body="The scheduled \`mls-tests\` run ended \`$conclusion\` on ${sha::8} (attempt $attempt): $url

${details:-No job failed; the run itself was $conclusion.}
A marathon failure uploads \`marathon-failing-op-sequence\`; promote it per docs/machine-checked-correctness-design.md §5.4."

echo "----"
echo "$body"
echo "----"

existing="$(gh issue list --repo "$GH_REPO" --state open --search "\"$TITLE\" in:title" --json number,title \
  --jq "map(select(.title == \"$TITLE\")) | .[0].number // empty")"
if [ -n "$DRY_RUN" ]; then
  if [ -n "$existing" ]; then
    echo "DRY_RUN: would comment on #$existing"
  else
    echo "DRY_RUN: would open a new issue"
  fi
  exit 0
fi
if [ -n "$existing" ]; then
  gh issue comment "$existing" --repo "$GH_REPO" --body "$body"
else
  gh issue create --repo "$GH_REPO" --title "$TITLE" --body "$body" --assignee "$NIGHTLY_OWNER"
fi
