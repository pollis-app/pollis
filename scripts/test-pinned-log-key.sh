#!/usr/bin/env bash
#
# test-pinned-log-key.sh — tests for the pinned-key agreement check (#945).
#
# A consistency gate is worth exactly as much as its ability to go red. This one
# guards a failure mode where the symptom is worse than the cause: a rotation
# that updates eight of nine surfaces leaves the ninth pinning a key the log no
# longer signs with, and that surface then reports an HONEST log as tampered
# (#732 did precisely this to pollis.com/artifacts). A check that quietly passed
# in that state would be worse than no check, because it would be cited.
#
# So every case below breaks one copy and asserts the checker names it. The
# baseline case at the top exists so a harness that copies nothing, or points at
# the wrong tree, cannot make the rest pass vacuously.
#
# Run: ./scripts/test-pinned-log-key.sh   (no arguments, no network)
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
  "scripts/check-pinned-log-key.py"
  "pollis-core/src/commands/transparency.rs"
  "website/artifacts.js"
  "website/learn.html"
  "website/assurance.html"
  "key-set.json"
  "SECURITY.md"
  "README.md"
  ".github/workflows/rebuild-verify.yml"
  ".github/workflows/verifier-release.yml"
)

# Build a pristine copy of the tree under $work/pristine.
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

run_check() { python3 "$1/scripts/check-pinned-log-key.py" 2>&1; }

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

# Flip one hex digit of the pinned SIGNER key wherever it appears in $2, inside
# tree $1. `mode` = full | prefix — the Learn page carries only a truncation.
corrupt_copy() {
  python3 - "$1" "$2" "$3" <<'PY'
import re, sys
tree, target, mode = sys.argv[1], sys.argv[2], sys.argv[3]
src = open(f"{tree}/pollis-core/src/commands/transparency.rs").read()
anchor = src.find("pub const PINNED_LOG_PUBLIC_KEYS")
key = re.search(r"\b[0-9a-f]{2624}\b", src[anchor:]).group(0)
needle = key if mode == "full" else key[:64]
# A single digit is the realistic typo, and the hardest case for the checker.
twisted = ("7" if needle[0] != "7" else "8") + needle[1:]
path = f"{tree}/{target}"
body = open(path).read()
assert needle in body, f"{target} does not carry the key to corrupt"
open(path, "w").write(body.replace(needle, twisted))
PY
}

echo "pinned-key agreement check (#945)"

# ── 0. The harness itself is not vacuous ────────────────────────────────────
expect_pass "$(fresh_tree baseline)" "an untouched tree passes"

# ── 1. Each copy, broken one at a time ──────────────────────────────────────
#
# This is the case the ticket is about: seven of eight updated, one missed.
for target in \
  "website/artifacts.js" \
  "key-set.json" \
  "SECURITY.md" \
  "README.md" \
  ".github/workflows/rebuild-verify.yml" \
  ".github/workflows/verifier-release.yml"
do
  tree="$(fresh_tree "$(echo "$target" | tr '/.' '__')")"
  corrupt_copy "$tree" "$target" full
  expect_fail "$tree" "$target" "a wrong key in $target is caught"
done

# The Learn page carries a 64-char truncation, not the whole key — a separate
# form, and therefore a separate way to be missed by a rotation.
tree="$(fresh_tree learn)"
corrupt_copy "$tree" "website/learn.html" prefix
expect_fail "$tree" "website/learn.html" "a wrong displayed prefix in learn.html is caught"

# ── 2. A rotation applied ONLY to the source of truth ───────────────────────
#
# The literal shape of the bug: someone rotates the key in Rust, ships it, and
# every other surface still pins the retired one.
tree="$(fresh_tree rotation)"
python3 - "$tree" <<'PY'
import re, sys
tree = sys.argv[1]
p = f"{tree}/pollis-core/src/commands/transparency.rs"
src = open(p).read()
anchor = src.find("pub const PINNED_LOG_PUBLIC_KEYS")
key = re.search(r"\b[0-9a-f]{2624}\b", src[anchor:]).group(0)
rotated = ("7" if key[0] != "7" else "8") + key[1:]
open(p, "w").write(src.replace(key, rotated))
PY
out="$(run_check "$tree")"
if [ "$(grep -c '::error::' <<<"$out")" -ge 6 ]; then
  ok "a rotation applied only to pollis-core fails every stale surface at once"
else
  bad "a rotation applied only to pollis-core should flag every other copy"
  echo "$out" | sed 's/^/         /'
fi

# ── 3. A key id that does not derive from its key ───────────────────────────
tree="$(fresh_tree keyid)"
python3 - "$tree" <<'PY'
import sys
tree = sys.argv[1]
p = f"{tree}/pollis-core/src/commands/transparency.rs"
src = open(p).read()
open(p, "w").write(src.replace('key_id: "6cbd4b2aed5c4bf1"', 'key_id: "6cbd4b2aed5c4bf0"', 1))
PY
expect_fail "$tree" "different key" "a key id that does not hash from its key is caught"

# ── 4. A NEW, unregistered copy ─────────────────────────────────────────────
#
# The check that keeps the registry honest: adding a ninth place without saying
# so must fail, or the next rotation misses it exactly as this ticket describes.
tree="$(fresh_tree ninth)"
python3 - "$tree" <<'PY'
import re, sys
tree = sys.argv[1]
src = open(f"{tree}/pollis-core/src/commands/transparency.rs").read()
anchor = src.find("pub const PINNED_LOG_PUBLIC_KEYS")
key = re.search(r"\b[0-9a-f]{2624}\b", src[anchor:]).group(0)
open(f"{tree}/docs-new-page.md", "w").write(f"# A ninth copy\n\n```\n{key}\n```\n")
PY
expect_fail "$tree" "not registered" "an unregistered ninth copy is caught"

# ── 5. A stale key nobody pinned ────────────────────────────────────────────
tree="$(fresh_tree stale)"
python3 - "$tree" <<'PY'
import sys
tree = sys.argv[1]
open(f"{tree}/leftover.md", "w").write("# Pre-rotation leftover\n\n" + "ab" * 1312 + "\n")
PY
expect_fail "$tree" "neither" "a retired key left behind in the tree is caught"

# ── 6. Order-independence of the source-of-truth lookup ─────────────────────
#
# `website-verify.yml` used to take the FIRST 2624-hex literal in the Rust file,
# which happens to be the signer only because the root const is declared below
# it. Swapping the two declarations must not change what this check asserts.
tree="$(fresh_tree order)"
python3 - "$tree" <<'PY'
import re, sys
tree = sys.argv[1]
p = f"{tree}/pollis-core/src/commands/transparency.rs"
src = open(p).read()
blocks = re.findall(
    r"pub const (?:PINNED_LOG_PUBLIC_KEYS|PINNED_LOG_ROOT_KEYS).*?\n\}\];\n",
    src, re.S,
)
assert len(blocks) == 2, blocks
signer, root = blocks
# Declare the root FIRST, so a positional reader would pick up the wrong key.
src = src.replace(signer, "@@SIGNER@@").replace(root, signer).replace("@@SIGNER@@", root)
open(p, "w").write(src)
PY
out="$(run_check "$tree")"
if grep -q "signer 6cbd4b2aed5c4bf1" <<<"$out"; then
  ok "the source-of-truth lookup is by const name, not by position"
else
  bad "swapping the two consts changed which key is treated as the signer"
  echo "$out" | sed 's/^/         /'
fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
