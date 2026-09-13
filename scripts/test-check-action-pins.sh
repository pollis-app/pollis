#!/usr/bin/env bash
#
# test-check-action-pins.sh — tests for scripts/check-action-pins.py.
#
# Every `uses:` in the workflows is pinned to a full commit SHA so that a retagged
# upstream action cannot run inside our release jobs. The pins only stay pinned if
# something refuses the next `@v4` that a copy-pasted step brings back, so each
# case below re-creates one shape of a mutable reference in a throwaway copy of
# the tree and asserts the checker names the offending file and ref. The baseline
# case at the top exists so a harness that copies nothing, or points at the wrong
# tree, cannot make the rest pass vacuously.
#
# Run: ./scripts/test-check-action-pins.sh   (no arguments, no network)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A throwaway tree with the checker and everything it reads, so the mutations
# below never touch the real repo.
fresh_tree() {
  local t="$work/$1"
  mkdir -p "$t/scripts" "$t/.github"
  cp "$repo/scripts/check-action-pins.py" "$t/scripts/"
  cp -R "$repo/.github/workflows" "$t/.github/workflows"
  if [ -d "$repo/.github/actions" ]; then
    cp -R "$repo/.github/actions" "$t/.github/actions"
  fi
  echo "$t"
}

run_checker() {
  python3 "$1/scripts/check-action-pins.py" 2>&1
}

# expect_fail TREE NEEDLE LABEL — the checker must exit non-zero AND name NEEDLE.
expect_fail() {
  local tree="$1" needle="$2" label="$3" out rc
  out="$(run_checker "$tree")"; rc=$?
  if [ "$rc" -ne 0 ] && grep -qF -- "$needle" <<<"$out"; then
    ok "$label"
  else
    bad "$label (rc=$rc)"
    echo "$out" | sed 's/^/        /'
  fi
}

expect_pass() {
  local tree="$1" label="$2" out rc
  out="$(run_checker "$tree")"; rc=$?
  if [ "$rc" -eq 0 ]; then
    ok "$label"
  else
    bad "$label (rc=$rc)"
    echo "$out" | sed 's/^/        /'
  fi
}

echo "check-action-pins.py:"

# ── 0. Baseline: the real tree passes ────────────────────────────────────────
tree="$(fresh_tree baseline)"
expect_pass "$tree" "the committed workflows and composite actions are all SHA-pinned"

# The file every mutation below edits, and a step that is definitely in it.
WF=".github/workflows/scripts-check.yml"
PINNED_CHECKOUT_RE='^      - uses: actions/checkout@[0-9a-f]\{40\} # .*$'
if ! grep -q "$PINNED_CHECKOUT_RE" "$repo/$WF"; then
  bad "test fixture: $WF has no SHA-pinned checkout step to mutate"
fi

# ── 1. A version tag ─────────────────────────────────────────────────────────
#
# The exact shape every action's README tells you to paste.
tree="$(fresh_tree tag)"
sed -i "s|$PINNED_CHECKOUT_RE|      - uses: actions/checkout@v4|" "$tree/$WF"
expect_fail "$tree" "actions/checkout@v4" "a \`@v4\` tag is refused and named"
expect_fail "$tree" "scripts-check.yml" "...with the file it is in"

# ── 2. A branch ──────────────────────────────────────────────────────────────
tree="$(fresh_tree branch)"
sed -i "s|$PINNED_CHECKOUT_RE|      - uses: actions/checkout@main|" "$tree/$WF"
expect_fail "$tree" "actions/checkout@main" "a branch ref is refused"

# ── 3. A short SHA ───────────────────────────────────────────────────────────
#
# Abbreviated SHAs are not content-addressed against collision by GitHub the way
# a full one is, and `git` will happily resolve a 7-char prefix to whichever
# object matches first.
tree="$(fresh_tree short)"
sed -i "s|$PINNED_CHECKOUT_RE|      - uses: actions/checkout@11d5960 # v4.4.0|" "$tree/$WF"
expect_fail "$tree" "actions/checkout@11d5960" "a short SHA is refused"

# ── 4. A full SHA with no version comment ────────────────────────────────────
#
# Dependabot rewrites the comment when it bumps a pin; without it a reviewer has
# to resolve the SHA by hand to know what version is running.
tree="$(fresh_tree nocomment)"
sed -i "s|$PINNED_CHECKOUT_RE|      - uses: actions/checkout@11d5960a326750d5838078e36cf38b85af677262|" "$tree/$WF"
expect_fail "$tree" "missing the" "a bare SHA without the version comment is refused"

# ── 5. A composite action under .github/actions ──────────────────────────────
#
# Composite actions call third-party actions too, and they are not under
# .github/workflows, so a checker that only globbed workflows would miss them.
tree="$(fresh_tree composite)"
mkdir -p "$tree/.github/actions/probe"
cat >"$tree/.github/actions/probe/action.yml" <<'YML'
name: probe
runs:
  using: composite
  steps:
    - uses: actions/upload-artifact@v4
YML
expect_fail "$tree" ".github/actions/probe/action.yml" "an unpinned action inside a composite action is refused"

# ── 6. An unpinned docker image ──────────────────────────────────────────────
tree="$(fresh_tree docker)"
sed -i "s|$PINNED_CHECKOUT_RE|      - uses: docker://alpine:3.20|" "$tree/$WF"
expect_fail "$tree" "docker://alpine:3.20" "a docker image without a digest is refused"

# ── 7. What must still pass ──────────────────────────────────────────────────
#
# Local actions ship in the same commit as their caller; a digest-pinned docker
# image is as immutable as a SHA; quoting the value is legal YAML.
tree="$(fresh_tree allowed)"
python3 - "$tree/$WF" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
extra = (
    "      - uses: ./.github/actions/desktop-e2e\n"
    "      - uses: docker://alpine@sha256:"
    "0000000000000000000000000000000000000000000000000000000000000000\n"
    "      - uses: 'actions/checkout@11d5960a326750d5838078e36cf38b85af677262' # v4.4.0\n"
)
open(path, "w").write(text.rstrip("\n") + "\n" + extra)
PY
expect_pass "$tree" "local, digest-pinned docker and quoted SHA-pinned refs are accepted"

# ── 8. An empty tree is a failure, not a pass ────────────────────────────────
tree="$work/empty"
mkdir -p "$tree/scripts" "$tree/.github/workflows"
cp "$repo/scripts/check-action-pins.py" "$tree/scripts/"
expect_fail "$tree" "no workflows found" "a tree with no workflows cannot pass vacuously"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
