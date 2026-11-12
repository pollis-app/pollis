#!/usr/bin/env bash
#
# test-sweep.sh — sweep.mjs's own tests (#1728).
#
# A drift check is worth exactly as much as its ability to go red. Each case
# copies every file the surface registry reads (sweep.mjs --list-files) into a
# throwaway git repo, breaks one thing, and asserts what sweep.mjs says:
#
#   - the pristine copy passes the registration check (so the rest cannot pass vacuously);
#   - a new public page nobody registered fails, naming the file;
#   - a registered page that was deleted fails (the other direction);
#   - a checked_by that disagrees with claims.json → audited_docs fails;
#   - an empty registry fails;
#   - a lane whose files are absent reports NOT BUILT, never PASS;
#   - the due list follows --since and the ledger, and --bump writes the ledger;
#   - an unknown dimension is a usage error.
#
# Run: ./scripts/test-sweep.sh   (no arguments, no network; not in CI, like every sweep)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

gitq() { git -C "$1" -c user.name=sweep-test -c user.email=sweep-test@invalid -c commit.gpgsign=false "${@:2}" >/dev/null 2>&1; }

mkdir -p "$work/pristine"
while IFS= read -r f; do
  mkdir -p "$work/pristine/$(dirname "$f")"
  cp "$repo/$f" "$work/pristine/$f"
done < <(node "$repo/scripts/sweep.mjs" --list-files)
for f in scripts/sweep.mjs scripts/check-install-env.py; do
  mkdir -p "$work/pristine/$(dirname "$f")"
  cp "$repo/$f" "$work/pristine/$f"
done
gitq "$work/pristine" init -q
gitq "$work/pristine" add -A
gitq "$work/pristine" commit -q -m pristine

fresh_tree() {
  local dest="$work/case-$1"
  rm -rf "$dest"
  cp -R "$work/pristine" "$dest"
  echo "$dest"
}

# run <tree> <args...> → sets $out and $rc
run() {
  local tree="$1"; shift
  out="$(node "$tree/scripts/sweep.mjs" --root "$tree" "$@" 2>&1)"
  rc=$?
}

# Edit a JSON file in place with a JS expression over `j`; fail loudly if the file is absent.
json_edit() {
  node -e '
    const fs = require("fs");
    const [file, expr] = process.argv.slice(1);
    const j = JSON.parse(fs.readFileSync(file, "utf8"));
    (new Function("j", expr))(j);
    fs.writeFileSync(file, JSON.stringify(j, null, 2) + "\n");
  ' "$1" "$2" || { echo "json_edit failed on $1" >&2; exit 2; }
}

echo "sweep.mjs self-test"

# 1. Pristine passes.
t="$(fresh_tree pristine)"
run "$t" surfaces
if [ "$rc" -eq 0 ] && grep -q "PASS     registration" <<<"$out"; then ok "pristine registry passes"; else bad "pristine registry should pass (rc=$rc)"; echo "$out"; fi

# 2. An unregistered page fails (untracked, then tracked).
t="$(fresh_tree unregistered)"
echo '<html><body>new</body></html>' > "$t/website/new-page.html"
run "$t" surfaces
if [ "$rc" -eq 1 ] && grep -q "website/new-page.html: public-content path not registered" <<<"$out"; then ok "unregistered page fails"; else bad "unregistered page should fail (rc=$rc)"; echo "$out"; fi
echo '# New topic' > "$t/learn/manim/scripts/new-topic.md"
gitq "$t" add -A
run "$t" surfaces
if [ "$rc" -eq 1 ] && grep -q "learn/manim/scripts/new-topic.md: public-content path not registered" <<<"$out"; then ok "unregistered /learn script fails"; else bad "unregistered /learn script should fail (rc=$rc)"; echo "$out"; fi

# 3. A registered page that no longer exists fails.
t="$(fresh_tree deleted)"
gitq "$t" rm -q website/faq.html
run "$t" surfaces
if [ "$rc" -eq 1 ] && grep -q 'content.website/faq: "website/faq.html" matches no tracked file' <<<"$out"; then ok "deleted registered page fails"; else bad "deleted registered page should fail (rc=$rc)"; echo "$out"; fi

# 4. checked_by must agree with claims.json → audited_docs.
t="$(fresh_tree checked-by)"
json_edit "$t/scripts/surfaces.json" 'j.content["website/faq"].checked_by = j.content["website/faq"].checked_by.filter((d) => d !== "claims");'
run "$t" surfaces
if [ "$rc" -eq 1 ] && grep -q 'website/faq.html is in claims.json audited_docs; add "claims"' <<<"$out"; then ok "checked_by disagreeing with claims.json fails"; else bad "checked_by disagreeing with claims.json should fail (rc=$rc)"; echo "$out"; fi

# 5. An empty registry fails.
t="$(fresh_tree empty)"
json_edit "$t/scripts/surfaces.json" 'j.content = {};'
run "$t" surfaces
if [ "$rc" -eq 1 ] && grep -q "empty registry" <<<"$out"; then ok "empty registry fails"; else bad "empty registry should fail (rc=$rc)"; echo "$out"; fi

# 6. A lane whose files are absent is NOT BUILT, in the report and the summary, never PASS.
t="$(fresh_tree not-built)"
run "$t" store-assets i18n-surfaces
if grep -q "store-assets (#1729) *NOT BUILT" <<<"$out" && grep -q "i18n-surfaces (#1724) *NOT BUILT" <<<"$out" && ! grep -q "PASS" <<<"$out"; then ok "missing lane files report NOT BUILT"; else bad "missing lane files should report NOT BUILT"; echo "$out"; fi
# A landed lane (#1731) is built and wired once its files exist.
t="$(fresh_tree built)"
for f in .claude/skills/reuse-check/SKILL.md scripts/check-clones.mjs scripts/clones.json scripts/clones-baseline.json; do
  mkdir -p "$t/$(dirname "$f")"
  cp "$repo/$f" "$t/$f"
done
run "$t" --list
if grep -q "^duplication .*#1731 *built" <<<"$out"; then ok "duplication is built once its files exist"; else bad "duplication should list as built"; echo "$out"; fi
run "$t" duplication
if ! grep -q "NOT BUILT\|UNWIRED" <<<"$out" && grep -q "node scripts/check-clones.mjs" <<<"$out"; then ok "duplication runs check-clones.mjs"; else bad "duplication should run check-clones.mjs"; echo "$out"; fi
run "$t" --summary
if [ "$rc" -eq 0 ] && grep -q "not yet built: .*store-assets (#1729)" <<<"$out"; then ok "summary names the unbuilt lanes"; else bad "summary should name the unbuilt lanes (rc=$rc)"; echo "$out"; fi

# 7. The due list follows --since and the ledger; --bump writes the ledger.
t="$(fresh_tree due)"
base="$(git -C "$t" rev-parse HEAD)"
echo '# touched' >> "$t/website/install.sh"
gitq "$t" commit -q -am "touch installer"
run "$t" installers --no-checks --since "$base"
if grep -q "installer or its env-var doc changed: website/install.sh" <<<"$out"; then ok "--since: a changed installer is due"; else bad "--since: a changed installer should be due"; echo "$out"; fi
run "$t" installers --no-checks --since HEAD
if grep -q "due      nothing since" <<<"$out"; then ok "--since HEAD: nothing due"; else bad "--since HEAD: nothing should be due"; echo "$out"; fi
json_edit "$t/scripts/sweep-ledger.json" "j.installers = { sha: \"$base\", date: \"2026-01-01\", by: \"test\" };"
run "$t" --summary
if grep -q "sweeps due: .*installers (1)" <<<"$out"; then ok "ledger: due since the recorded sha"; else bad "ledger: installers should be due since the recorded sha"; echo "$out"; fi
run "$t" --bump installers --at HEAD --by test
head_sha="$(git -C "$t" rev-parse HEAD)"
if [ "$rc" -eq 0 ] && [ "$(node -p "require('$t/scripts/sweep-ledger.json').installers.sha")" = "$head_sha" ]; then ok "--bump records the sweep in the ledger"; else bad "--bump should set installers.sha to HEAD (rc=$rc)"; echo "$out"; fi
run "$t" --summary
if ! grep -q "installers (" <<<"$out"; then ok "after --bump: nothing due"; else bad "after --bump nothing should be due"; echo "$out"; fi
echo '<html><body>more</body></html>' > "$t/website/more.html"
json_edit "$t/scripts/surfaces.json" 'j.content["website/more"] = { kind: "website-page", source: "website/more.html", checked_by: [] };'
gitq "$t" add -A
gitq "$t" commit -q -m "add a registered page"
run "$t" surfaces --since "$base"
if [ "$rc" -eq 0 ] && grep -q "1 public-content file(s) added or renamed: .*website/more.html" <<<"$out"; then ok "a newly registered page passes and is due for review"; else bad "a newly registered page should pass and be listed as due (rc=$rc)"; echo "$out"; fi

# 8. Usage errors.
t="$(fresh_tree usage)"
run "$t" no-such-dimension
if [ "$rc" -eq 2 ]; then ok "unknown dimension is a usage error"; else bad "unknown dimension should exit 2 (rc=$rc)"; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
