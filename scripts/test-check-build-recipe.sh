#!/usr/bin/env bash
#
# test-check-build-recipe.sh — tests for the build-job environment check in
# scripts/check-build-recipe.py.
#
# The release build jobs used to write the account-wide R2 write key and the
# LiveKit API secret into $GITHUB_ENV with no consumer in the job — visible to
# every crate build.rs, proc-macro, pnpm lifecycle script and third-party action
# that ran afterwards. Nothing checked for it, so it survived the #987 sweep that
# removed the database credentials from the very same step. The checker now
# refuses any build-job export outside the option_env! recipe (plus the toolchain
# flags) and any `secrets.*` export through $GITHUB_ENV anywhere in the release
# workflows; each case below re-creates one shape of that mistake in a throwaway
# copy of the tree and asserts the checker names the offending variable.
#
# The baseline case at the top exists so a harness that copies nothing, or points
# at the wrong tree, cannot make the rest pass vacuously.
#
# Run: ./scripts/test-check-build-recipe.sh   (no arguments, no network)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Every file the checker reads, plus the checker. Copied into a throwaway tree so
# the mutations below never touch the real repo.
FILES=(
  "scripts/check-build-recipe.py"
  "pollis-core/src/config.rs"
  ".github/workflows/desktop-release.yml"
  ".github/workflows/cli-release.yml"
  ".github/workflows/rebuild-verify.yml"
)

mkdir -p "$work/pristine"
for f in "${FILES[@]}"; do
  mkdir -p "$work/pristine/$(dirname "$f")"
  cp "$repo/$f" "$work/pristine/$f"
done

# A fresh tree for one case. $1 = case name; echoes the tree path.
fresh_tree() {
  local dest="$work/case-$1"
  rm -rf "$dest"
  cp -r "$work/pristine" "$dest"
  echo "$dest"
}

run_check() { python3 "$1/scripts/check-build-recipe.py" 2>&1; }

# Assert the checker FAILS on $1 and that its output mentions $2.
expect_fail() {
  local tree="$1" needle="$2" name="$3" out rc
  out="$(run_check "$tree")"; rc=$?
  if [ "$rc" -eq 0 ]; then
    bad "$name (checker passed; it must have failed)"
    return
  fi
  if ! grep -qF -- "$needle" <<<"$out"; then
    bad "$name (failed, but never named '$needle')"
    echo "$out" | sed 's/^/         /'
    return
  fi
  ok "$name"
}

expect_pass() {
  local tree="$1" name="$2" out rc
  out="$(run_check "$tree")"; rc=$?
  if [ "$rc" -ne 0 ]; then
    bad "$name (checker failed; it must have passed)"
    echo "$out" | sed 's/^/         /'
    return
  fi
  ok "$name"
}

# Insert $3 (one line, already indented for a `run: |` body) directly after the
# LAST `>> $GITHUB_ENV` line inside job $2 of workflow $1 in tree $4 — i.e. into
# the job's existing recipe-loading step.
inject_after_last_export() {
  python3 - "$4/$1" "$2" "$3" <<'PY'
import re, sys
path, job, line = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
m = re.search(rf"^  {re.escape(job)}:$(.*?)(?=^  [a-z][a-z0-9_-]*:$|\Z)", text, re.S | re.M)
assert m, f"job {job} not found in {path}"
block = m.group(1)
idx = block.rfind("GITHUB_ENV")
assert idx >= 0, f"job {job} has no $GITHUB_ENV export to anchor on"
eol = block.index("\n", idx)
block = block[: eol + 1] + line + "\n" + block[eol + 1 :]
open(path, "w").write(text[: m.start(1)] + block + text[m.end(1) :])
PY
}

echo "build-job environment check (check-build-recipe.py)"

# ── 0. The harness itself is not vacuous ────────────────────────────────────
expect_pass "$(fresh_tree baseline)" "an untouched tree passes"

# ── 1. The literal shape of the finding, one build job at a time ────────────
#
# The four credentials that sat in every build job's environment, re-added to
# each build job in turn. Every one must be named.
for spec in \
  ".github/workflows/desktop-release.yml build-macos R2_SECRET_KEY" \
  ".github/workflows/desktop-release.yml build-windows R2_ACCESS_KEY_ID" \
  ".github/workflows/desktop-release.yml build-linux LIVEKIT_API_SECRET" \
  ".github/workflows/cli-release.yml build-cli-linux LIVEKIT_API_KEY" \
  ".github/workflows/cli-release.yml build-cli-macos R2_SECRET_KEY" \
  ".github/workflows/cli-release.yml build-cli-windows R2_ACCESS_KEY_ID"
do
  read -r wf job var <<<"$spec"
  tree="$(fresh_tree "$(echo "${job}_${var}" | tr '/.' '__')")"
  inject_after_last_export "$wf" "$job" \
    "          echo \"${var}=\${{ secrets.${var} }}\" >> \$GITHUB_ENV" "$tree"
  expect_fail "$tree" "$var" "${var} exported by ${job} ($(basename "$wf")) is caught"
done

# A non-secret name the binary never reads is still refused: the rule is "the
# recipe and nothing else", not a denylist of known credentials.
tree="$(fresh_tree account_id)"
inject_after_last_export ".github/workflows/desktop-release.yml" "build-linux" \
  '          echo "CLOUDFLARE_ACCOUNT_ID=${{ secrets.CLOUDFLARE_ACCOUNT_ID }}" >> $GITHUB_ENV' "$tree"
expect_fail "$tree" "CLOUDFLARE_ACCOUNT_ID" "an export the client never reads is caught even when it is not a credential"

# ── 2. A secret smuggled under an allowed toolchain name ────────────────────
tree="$(fresh_tree smuggled)"
inject_after_last_export ".github/workflows/desktop-release.yml" "build-linux" \
  '          echo "RUSTFLAGS=${{ secrets.R2_SECRET_KEY }}" >> $GITHUB_ENV' "$tree"
expect_fail "$tree" "RUSTFLAGS" "a secrets.* value under a toolchain name is caught"

# ── 3. The toolchain flags themselves still pass ────────────────────────────
#
# SOURCE_DATE_EPOCH / RUSTFLAGS / CFLAGS / CXXFLAGS are exported by the real
# build jobs; the allow-list must not reject the exports it exists to permit.
tree="$(fresh_tree toolchain)"
inject_after_last_export ".github/workflows/desktop-release.yml" "build-macos" \
  '          echo "SOURCE_DATE_EPOCH=$(git log -1 --format=%ct)" >> $GITHUB_ENV' "$tree"
expect_pass "$tree" "a toolchain flag export is allowed"

# ── 4. A credential loaded into a publish job's $GITHUB_ENV ─────────────────
#
# The release/provenance jobs DO need the R2 key, but only on their `aws s3`
# steps. Loading it job-wide (the shape that shipped) is refused: every other
# step in the job would inherit it.
for spec in \
  ".github/workflows/desktop-release.yml release" \
  ".github/workflows/desktop-release.yml provenance" \
  ".github/workflows/cli-release.yml publish-cli" \
  ".github/workflows/cli-release.yml provenance-cli"
do
  read -r wf job <<<"$spec"
  tree="$(fresh_tree "publish_${job}")"
  python3 - "$tree/$wf" "$job" <<'PY'
import re, sys
path, job = sys.argv[1], sys.argv[2]
text = open(path).read()
m = re.search(rf"^  {re.escape(job)}:$(.*?)(?=^  [a-z][a-z0-9_-]*:$|\Z)", text, re.S | re.M)
assert m, f"job {job} not found"
block = m.group(1)
# The checkout step is SHA-pinned with a trailing version comment, so match the
# action name rather than a literal ref that dependabot rewrites.
anchor = re.search(r"^      - uses: actions/checkout@\S+.*\n", block, re.M)
assert anchor, "checkout step not found"
i = anchor.end()
step = (
    "\n      - name: Load production secrets\n"
    "        run: |\n"
    '          echo "R2_SECRET_KEY=${{ secrets.R2_SECRET_KEY }}" >> $GITHUB_ENV\n'
)
block = block[:i] + step + block[i:]
open(path, "w").write(text[: m.start(1)] + block + text[m.end(1) :])
PY
  expect_fail "$tree" "R2_SECRET_KEY" "a job-wide R2 credential in ${job} ($(basename "$wf")) is caught"
done

# ── 5. A new build job the checker does not know about ──────────────────────
#
# Adding a `build-*` job without listing it in BUILD_JOBS would put it outside
# the check entirely, which is how the original exports went unnoticed.
tree="$(fresh_tree newjob)"
python3 - "$tree/.github/workflows/cli-release.yml" <<'PY'
import sys
path = sys.argv[1]
text = open(path).read()
text += (
    "\n  build-cli-freebsd:\n"
    "    runs-on: ubuntu-latest\n"
    "    steps:\n"
    "      - run: echo hi\n"
)
open(path, "w").write(text)
PY
expect_fail "$tree" "build-cli-freebsd" "an unlisted build-* job is caught"

# ── 6. An export in a shape the checker cannot read ─────────────────────────
#
# A write it cannot parse is a write it cannot vouch for; it must fail rather
# than silently pass over it.
tree="$(fresh_tree opaque)"
inject_after_last_export ".github/workflows/desktop-release.yml" "build-linux" \
  '          cat /tmp/extra.env >> $GITHUB_ENV' "$tree"
expect_fail "$tree" "cannot read what this line exports" "an unparseable \$GITHUB_ENV write is refused"

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
