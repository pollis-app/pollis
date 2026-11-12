#!/usr/bin/env bash
#
# test-check-clones.sh — the clone ratchet's own tests (#1731).
#
# A ratchet is worth exactly as much as its ability to go red. This copies the
# tracked files of every boundary in scripts/clones.json into a throwaway tree
# and asserts:
#
#   - the pristine copy passes (so the rest cannot pass vacuously);
#   - a newly pasted copy of existing code fails, in EACH boundary, naming the
#     new file;
#   - moving baselined code (lines inserted above it) does not fail;
#   - a boundary that scans nothing fails, and so does a configured path that
#     no longer exists;
#   - a clone between two copies inside an inline Rust `#[cfg(test)] mod` is
#     not reported, while the same clone in shipped code is.
#
# Run: ./scripts/test-check-clones.sh   (needs pnpm; jscpd comes via pnpm dlx)
# Not CI, like the check itself; run it after changing check-clones.mjs.
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/pristine/scripts"
cp "$repo/scripts/check-clones.mjs" "$repo/scripts/clones.json" "$repo/scripts/clones-baseline.json" "$work/pristine/scripts/"
paths="$(node -e 'const c=require(process.argv[1]);for(const b of Object.values(c.boundaries)){for(const p of b.paths){console.log(p)}}' "$repo/scripts/clones.json")"
# shellcheck disable=SC2086
(cd "$repo" && git ls-files -z -- $paths) | rsync -a --from0 --files-from=- "$repo/" "$work/pristine/"

fresh_tree() {
  local dest="$work/case-$1"
  rm -rf "$dest"
  cp -R "$work/pristine" "$dest"
  echo "$dest"
}

# run <tree> [args…] → sets $out and $rc
run() {
  local tree="$1"
  shift
  out="$(node "$tree/scripts/check-clones.mjs" --root "$tree" "$@" 2>&1)"
  rc=$?
}

echo "pristine tree"
run "$work/pristine"
if [ "$rc" -eq 0 ]; then ok "passes against the committed baseline"; else bad "pristine tree fails:"; echo "$out"; fi

echo "a pasted copy fails in each boundary"
# boundary  source-to-copy  pasted-copy
while read -r boundary src dst; do
  t="$(fresh_tree "paste-$boundary")"
  cp "$t/$src" "$t/$dst"
  run "$t" --boundary "$boundary"
  if [ "$rc" -ne 0 ] && [[ "$out" == *"NEW"* ]] && [[ "$out" == *"$dst"* ]]; then
    ok "$boundary: $dst is reported as new"
  else
    bad "$boundary: pasted $dst was not reported (rc=$rc)"; echo "$out"
  fi
done <<'CASES'
frontend frontend/src/utils/drafts.ts frontend/src/utils/draftsPasted.ts
mobile mobile/lib/links.ts mobile/lib/linksPasted.ts
website website/verify-downloads.js website/verify-downloads-pasted.js
rust pollis-core/src/commands/r2.rs pollis-core/src/commands/r2_pasted.rs
CASES

echo "moved code is not new"
t="$(fresh_tree moved)"
python3 - "$t/frontend/src/App.tsx" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
open(p, "w").write("// moved\n" * 40 + s)
EOF
run "$t" --boundary frontend
if [ "$rc" -eq 0 ]; then ok "40 lines inserted above baselined clones still pass"; else bad "moved code reported as new:"; echo "$out"; fi

echo "a reshaped baseline clone is not new, an extra clone in the same file pair is"
t="$(fresh_tree reshaped)"
# Edit the edge of a baselined website clone (page chrome shared by every page).
python3 - "$t/website/artifacts.html" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read()
anchor = '  <script src="artifacts.js"></script>'
assert anchor in s, "anchor missing"
open(p, "w").write(s.replace(anchor, '  <script src="reshaped.js"></script>\n' + anchor, 1))
EOF
run "$t" --boundary website
if [ "$rc" -eq 0 ]; then ok "an edit at the edge of baselined page chrome passes"; else bad "reshaped clone reported as new:"; echo "$out"; fi
t="$(fresh_tree same-pair)"
pasted='export function pastedSamePair(items: readonly string[], limit: number): string[] {
  const out: string[] = [];
  for (const item of items) {
    const trimmed = item.trim().toLowerCase();
    if (trimmed.length === 0 || out.includes(trimmed)) { continue; }
    out.push(trimmed.length > limit ? trimmed.slice(0, limit) + "…" : trimmed);
  }
  return out.sort((a, b) => a.localeCompare(b));
}'
printf '\n%s\n' "$pasted" >> "$t/mobile/hooks/useEnrollmentApprovals.ts"
printf '\n%s\n' "$pasted" >> "$t/mobile/hooks/useHistoryTransferService.ts"
run "$t" --boundary mobile
if [ "$rc" -ne 0 ] && [[ "$out" == *"useEnrollmentApprovals.ts"* ]]; then ok "a new clone between two already-paired files fails"; else bad "extra clone in a baselined pair passed (rc=$rc)"; echo "$out"; fi

echo "a scan of nothing fails"
t="$(fresh_tree empty)"
node -e 'const f=process.argv[1];const fs=require("fs");const c=JSON.parse(fs.readFileSync(f));c.boundaries.website.format=["cobol"];fs.writeFileSync(f,JSON.stringify(c))' "$t/scripts/clones.json"
run "$t" --boundary website
if [ "$rc" -ne 0 ] && [[ "$out" == *"scanned no files"* ]]; then ok "website with no matching format fails"; else bad "empty scan passed (rc=$rc)"; echo "$out"; fi

t="$(fresh_tree missing)"
rm -rf "$t/mobile/theme"
run "$t" --boundary mobile
if [ "$rc" -ne 0 ] && [[ "$out" == *"mobile/theme: configured path does not exist"* ]]; then ok "a missing configured path fails"; else bad "missing path passed (rc=$rc)"; echo "$out"; fi

echo "inline Rust test modules"
body='    pub fn pasted_helper(input: &[u8]) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(input.len() * 2);
        for (i, b) in input.iter().enumerate() {
            if i % 3 == 0 { out.push(b.wrapping_add(7)); } else { out.push(b ^ 0x5a); }
            out.push(b.rotate_left((i % 8) as u32));
        }
        if out.is_empty() { return Err(String::from("empty input to pasted helper")); }
        Ok(out)
    }'
t="$(fresh_tree inline-tests)"
printf '\n#[cfg(test)]\nmod pasted_a {\n%s\n}\n' "$body" >> "$t/pollis-api/src/lib.rs"
printf '\n#[cfg(test)]\nmod pasted_b {\n%s\n}\n' "$body" >> "$t/pollis-schema/src/lib.rs"
run "$t" --boundary rust
if [ "$rc" -eq 0 ]; then ok "a clone between two inline test modules is not reported"; else bad "inline test clone reported:"; echo "$out"; fi
t="$(fresh_tree shipped)"
printf '\npub mod pasted_a {\n%s\n}\n' "$body" >> "$t/pollis-api/src/lib.rs"
printf '\npub mod pasted_b {\n%s\n}\n' "$body" >> "$t/pollis-schema/src/lib.rs"
run "$t" --boundary rust
if [ "$rc" -ne 0 ] && [[ "$out" == *"pollis-api/src/lib.rs"* ]]; then ok "the same clone in shipped code is reported"; else bad "shipped clone not reported (rc=$rc)"; echo "$out"; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
