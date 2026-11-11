#!/usr/bin/env bash
#
# test-check-facts.sh — the facts registry's own tests (#1722).
#
# A consistency gate is worth exactly as much as its ability to go red. Each
# case below copies every file scripts/facts.json reads into a throwaway tree,
# breaks one thing, and asserts what the checker says about it:
#
#   - the pristine copy passes (so the rest cannot pass vacuously);
#   - a value changed in CODE fails, naming the fact and the stale copy;
#   - a value changed in COPY fails;
#   - copy reworded so the pattern no longer matches fails as unanchored;
#   - code that moved so value_from extracts nothing fails;
#   - two value_from sources that disagree fail;
#   - the same break on a `disputed` fact warns and exits 0;
#   - an empty registry fails;
#   - a registry file outside scripts-check.yml's paths filter fails;
#   - number words and digit forms are interchangeable ("twelve" == "12").
#
# Run: ./scripts/test-check-facts.sh   (no arguments, no network)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/pristine"
while IFS= read -r f; do
  mkdir -p "$work/pristine/$(dirname "$f")"
  cp "$repo/$f" "$work/pristine/$f"
done < <(node "$repo/scripts/check-facts.mjs" --list-files)
for f in scripts/check-facts.mjs scripts/facts.json .github/workflows/scripts-check.yml; do
  mkdir -p "$work/pristine/$(dirname "$f")"
  cp "$repo/$f" "$work/pristine/$f"
done

fresh_tree() {
  local dest="$work/case-$1"
  rm -rf "$dest"
  cp -r "$work/pristine" "$dest"
  echo "$dest"
}

# run <tree> → sets $out and $rc
run() {
  out="$(node "$1/scripts/check-facts.mjs" --root "$1" 2>&1)"
  rc=$?
}

# Replace the first occurrence of a literal string in a file; fail the case loudly if absent,
# so a mutation that silently does nothing cannot make a case pass.
mutate() {
  local file="$1" from="$2" to="$3"
  python3 - "$file" "$from" "$to" <<'EOF' || { echo "mutation target not found in $1: $2" >&2; exit 2; }
import sys
p, a, b = sys.argv[1:]
s = open(p, encoding="utf-8").read()
if a not in s:
    sys.exit(1)
open(p, "w", encoding="utf-8").write(s.replace(a, b, 1))
EOF
}

echo "check-facts.mjs:"

t="$(fresh_tree baseline)"; run "$t"
if [ "$rc" -eq 0 ] && [[ "$out" == *"Facts OK"* ]]; then ok "pristine copy passes"; else bad "pristine copy should pass: $out"; fi

t="$(fresh_tree code)"
mutate "$t/pollis-core/src/commands/pin.rs" "MAX_FAILED_ATTEMPTS: u32 = 10;" "MAX_FAILED_ATTEMPTS: u32 = 11;"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[pin-max-failed-attempts] states \"ten\" but 11"* ]] && [[ "$out" == *"website/security.html:"* ]]; then
  ok "a value changed in code fails and names the stale copy"
else bad "code change should fail on pin-max-failed-attempts: $out"; fi

t="$(fresh_tree copy)"
mutate "$t/docs/security-whitepaper.md" "TTL 10 minutes; deleted" "TTL 15 minutes; deleted"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"docs/security-whitepaper.md:"*"[otp-ttl-minutes] states \"15\" but 10 minutes"* ]]; then
  ok "a value changed in copy fails with file:line"
else bad "copy change should fail on otp-ttl-minutes: $out"; fi

t="$(fresh_tree reword)"
mutate "$t/website/learn.html" "Three kinds of loss are expected. Two come" "There are three kinds of expected loss. Two come"
run "$t"
# The second sentence on /learn still anchors the fact, so rewording one copy passes;
# rewording every copy must not.
if [ "$rc" -eq 0 ]; then ok "rewording one of two anchored copies still passes"; else bad "rewording one of two copies should pass: $out"; fi
mutate "$t/website/learn.html" "What we do promise is narrow and firm. Three kinds of loss are expected" "What we do promise is narrow and firm. There are three kinds of expected loss"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[accepted-loss-count] assertion /{value} kinds of loss are expected/ not found"* ]]; then
  ok "reworded copy fails as unanchored"
else bad "reworded copy should fail as not found: $out"; fi

t="$(fresh_tree moved)"
mutate "$t/pollis-delivery/src/reports.rs" "pub const REPORTS_PER_DAY" "pub const REPORT_BUDGET"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[reports-per-day] value_from pattern"*"matched nothing"* ]]; then
  ok "code that moved fails instead of passing on nothing"
else bad "renamed constant should fail: $out"; fi

t="$(fresh_tree sources)"
mutate "$t/pollis-delivery/src/messages.rs" '"-12 months"' '"-6 months"'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[watermark-stale-months] value_from sources disagree"* ]]; then
  ok "a code default that disagrees with prod config fails"
else bad "disagreeing value_from sources should fail: $out"; fi

t="$(fresh_tree disputed)"
mutate "$t/pollis-core/src/commands/pin.rs" "MAX_FAILED_ATTEMPTS: u32 = 10;" "MAX_FAILED_ATTEMPTS: u32 = 11;"
python3 - "$t/scripts/facts.json" <<'EOF'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["facts"]["pin-max-failed-attempts"]["status"] = "disputed"
d["facts"]["pin-max-failed-attempts"]["note"] = "test"
json.dump(d, open(p, "w"), indent=2)
EOF
run "$t"
if [ "$rc" -eq 0 ] && [[ "$out" == *"⚠"*"[pin-max-failed-attempts] states \"ten\" but 11"*"disputed: test"* ]]; then
  ok "a disputed fact warns with both values and exits 0"
else bad "disputed fact should warn, not fail: $out"; fi

t="$(fresh_tree empty)"
echo '{"_comment": [], "facts": {}}' > "$t/scripts/facts.json"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"no facts declared"* ]]; then ok "an empty registry fails"; else bad "empty registry should fail: $out"; fi

t="$(fresh_tree trigger)"
mutate "$t/.github/workflows/scripts-check.yml" '      - "learn/**"' '      - "learn-nothing/**"'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[trigger-paths] learn/manim/"*"no pull_request path"* ]]; then
  ok "a registry file outside the workflow's paths filter fails"
else bad "uncovered trigger path should fail: $out"; fi

t="$(fresh_tree words)"
mutate "$t/website/retention.html" "<strong>twelve months</strong>" "<strong>12 months</strong>"
run "$t"
if [ "$rc" -eq 0 ]; then ok "\"12\" and \"twelve\" are the same value"; else bad "digit form should satisfy a word-form fact: $out"; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
