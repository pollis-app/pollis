#!/usr/bin/env bash
# Claude Code hook (#1347): put the release check in front of the model at the
# two moments it matters. It is advisory only: it adds context and never
# blocks, so its exit status is always 0.
#
#   PostToolUse(Bash) after `gh pr merge <N>`   → the PR's blast radius
#   PreToolUse(Bash)  before a deploy / release → which targets are stale
#
# Reads the hook payload on stdin, filters on tool_input.command itself.

set -uo pipefail

payload="$(cat)"
event="$(printf '%s' "$payload" | jq -r '.hook_event_name // empty' 2>/dev/null)"
cmd="$(printf '%s' "$payload" | jq -r '.tool_input.command // empty' 2>/dev/null)"
root="${CLAUDE_PROJECT_DIR:-$(git rev-parse --show-toplevel 2>/dev/null)}"
[ -n "$cmd" ] && [ -n "$root" ] || exit 0
cd "$root" || exit 0

emit() {
  jq -n --arg e "$event" --arg c "$1" \
    '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
  exit 0
}

case "$event" in
  PostToolUse)
    # A merge, and not a failed one: gh prints nothing useful to key on, so
    # ask GitHub whether the PR is merged now.
    if printf '%s' "$cmd" | grep -qE '(^|[;&|[:space:]])gh pr merge([[:space:]]|$)'; then
      pr="$(printf '%s' "$cmd" | grep -oE 'gh pr merge[[:space:]]+#?[0-9]+' | grep -oE '[0-9]+$' | head -1)"
      [ -n "$pr" ] || exit 0
      state="$(perl -e 'alarm 15; exec @ARGV' gh pr view "$pr" --json state -q .state 2>/dev/null)"
      [ "$state" = "MERGED" ] || exit 0
      impact="$(perl -e 'alarm 60; exec @ARGV' scripts/release-impact.sh --pr "$pr" 2>/dev/null)"
      emit "PR #$pr is merged. Release check (advisory): run /release-check, or at least decide deploy, defer or N/A for each target below, and confirm its linked issues are closed.
$impact"
    fi
    ;;
  PreToolUse)
    # Deploy and release entry points: dispatching a deploy/release workflow,
    # or pushing a release tag.
    deploy=""
    if printf '%s' "$cmd" | grep -qE 'gh workflow run[[:space:]]+[^ ]*(delivery-deploy|desktop-release|cli-release|mobile-apk-release|mobile-ota-release|verifier-release|transparency-publish|livekit-deploy|website-deploy|relay-image)'; then
      deploy=1
    fi
    if printf '%s' "$cmd" | grep -qE 'git push[^;&|]*[[:space:]](refs/tags/)?(v[0-9]|mobile-v[0-9]|mobile-ota-|pollis-verify-v[0-9])'; then
      deploy=1
    fi
    [ -n "$deploy" ] || exit 0
    impact="$(perl -e 'alarm 45; exec @ARGV' scripts/release-impact.sh --brief 2>/dev/null)"
    last="$(git tag -l 'v*' 'mobile-v*' --sort=-creatordate | head -1)"
    parity=""
    if [ -n "$last" ]; then
      parity="$(perl -e 'alarm 30; exec @ARGV' node scripts/feature-parity.mjs --since "$last" 2>/dev/null | sed -n '1,25p')"
    fi
    emit "About to deploy or release. Release check (advisory, not blocking): make sure this isn't shipping one target while leaving a dependent one stale, or a feature on one client only.
$impact

$parity"
    ;;
esac
exit 0
