#!/usr/bin/env bash
#
# test-check-claims.sh — the claims ledger's own tests (#1723).
#
# A consistency gate is worth exactly as much as its ability to go red. Each
# case copies every file scripts/claims.json reads into a throwaway tree, breaks
# one thing, and asserts what check-claims.mjs (or check-learn-sync.mjs) says:
#
#   - the pristine copy passes (so the rest cannot pass vacuously);
#   - DELETING the file a claim is anchored to fails, naming the claim;
#   - REWORDING the sentence a claim quotes fails, naming the claim;
#   - a glob anchor whose last matching file is deleted fails;
#   - an unknown `fact` id, a bad kind and a duplicate id fail;
#   - an empty ledger fails;
#   - an audited doc with no claims left fails;
#   - a /learn caption cue or transcript paragraph that drifts from its
#     narration script fails, and `--write` brings it back in sync.
#
# Run by hand (not in CI; sweep checks never gate a merge):
#   ./scripts/test-check-claims.sh   (no arguments, no network)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"

pass=0; fail=0
ok()  { pass=$((pass+1)); echo "  ok   — $1"; }
bad() { fail=$((fail+1)); echo "  FAIL — $1"; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

copy_in() {
  mkdir -p "$work/pristine/$(dirname "$1")"
  cp "$repo/$1" "$work/pristine/$1"
}
while IFS= read -r f; do copy_in "$f"; done < <(node "$repo/scripts/check-claims.mjs" --list-files)
for f in scripts/check-claims.mjs scripts/check-learn-sync.mjs scripts/lib/doc-text.mjs scripts/claims.json \
         scripts/facts.json website/learn.html; do
  copy_in "$f"
done
for f in "$repo"/learn/manim/scripts/*.md "$repo"/website/learn/media/*.vtt; do copy_in "${f#"$repo"/}"; done

fresh_tree() {
  local dest="$work/case-$1"
  rm -rf "$dest"
  cp -r "$work/pristine" "$dest"
  echo "$dest"
}

# run <tree> [checker] → sets $out and $rc
run() {
  out="$(node "$1/scripts/${2:-check-claims.mjs}" --root "$1" 2>&1)"
  rc=$?
}

# Replace the first occurrence of a literal string in a file; abort loudly if absent,
# so a mutation that silently does nothing cannot make a case pass.
mutate() {
  python3 - "$1" "$2" "$3" <<'EOF' || { echo "mutation target not found in $1: $2" >&2; exit 2; }
import sys
p, a, b = sys.argv[1:]
s = open(p, encoding="utf-8").read()
if a not in s:
    sys.exit(1)
open(p, "w", encoding="utf-8").write(s.replace(a, b, 1))
EOF
}

# edit_ledger <tree> <python expression on d> — rewrite the tree's claims.json.
edit_ledger() {
  python3 - "$1/scripts/claims.json" "$2" <<'EOF'
import json, sys
p, code = sys.argv[1:]
d = json.load(open(p))
exec(code)
json.dump(d, open(p, "w"), indent=2)
EOF
}

# A claim anchored to exactly one plain file, which no other claim also anchors,
# and whose quote lives in a doc (not the anchor) — so deleting the file breaks that anchor only.
pick() {
  node -e '
    const d = require(process.argv[1]);
    const want = process.argv[2];
    const count = {};
    for (const c of d.claims) for (const a of c.anchors) count[a] = (count[a] || 0) + 1;
    const plain = d.claims.find((c) => c.anchors.some((a) => !/[*?]/.test(a) && /\.\w+$/.test(a) && a !== c.doc));
    const glob = d.claims.find((c) => c.anchors.some((a) => /[*?]/.test(a)));
    if (want === "plain") { console.log(plain.id + "\t" + plain.anchors.find((a) => !/[*?]/.test(a) && /\.\w+$/.test(a) && a !== plain.doc)); }
    if (want === "glob") { console.log(glob.id + "\t" + glob.anchors.find((a) => /[*?]/.test(a))); }
    if (want === "quote") { const c = d.claims.find((c) => c.doc.endsWith(".md") && c.locator.quote.split(" ").length >= 5); console.log(c.id + "\t" + c.doc + "\t" + c.locator.quote); }
  ' "$work/pristine/scripts/claims.json" "$1"
}

echo "check-claims.mjs:"

t="$(fresh_tree pristine-run)"
run "$t"
if [ "$rc" -eq 0 ]; then ok "the pristine copy passes"; else bad "pristine copy should pass: $out"; fi

IFS=$'\t' read -r cid anchor < <(pick plain)
t="$(fresh_tree deleted-anchor)"
rm -f "$t/$anchor"
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[$cid] anchor $anchor matches no file"* ]]; then
  ok "deleting $anchor fails [$cid]"
else bad "deleted anchor should fail for $cid: $out"; fi

IFS=$'\t' read -r gid glob < <(pick glob)
t="$(fresh_tree deleted-glob)"
python3 - "$t" "$glob" <<'EOF'
import os, re, sys
root, glob = sys.argv[1:]
rx = ""
i = 0
while i < len(glob):
    if glob[i:i+2] == "**":
        rx += ".*"; i += 2; continue
    rx += "[^/]*" if glob[i] == "*" else ("[^/]" if glob[i] == "?" else re.escape(glob[i]))
    i += 1
r = re.compile("^" + rx + "$")
for d, _, fs in os.walk(root):
    for f in fs:
        rel = os.path.relpath(os.path.join(d, f), root)
        if r.match(rel):
            os.remove(os.path.join(d, f))
EOF
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[$gid] anchor $glob matches no file"* ]]; then
  ok "deleting every file under $glob fails [$gid]"
else bad "emptied glob anchor should fail for $gid: $out"; fi

IFS=$'\t' read -r qid qdoc quote < <(pick quote)
t="$(fresh_tree reworded)"
# Swap the quote's last word for another: the sentence still reads, but says something else.
last="${quote##* }"
python3 - "$t/$qdoc" "$quote" <<'EOF' || { echo "quote not found verbatim in $qdoc; picking by normalised text is not supported here" >&2; }
import re, sys
p, q = sys.argv[1:]
s = open(p, encoding="utf-8").read()
words = q.split(" ")
# match the quote across any whitespace/`**` the source uses
pat = r"(?:\*\*)?\s*".join(re.escape(w) for w in words)
m = re.search(pat, s)
if not m:
    sys.exit(1)
seg = m.group(0)
i = seg.rfind(words[-1])
s = s[:m.start()] + seg[:i] + "REWORDED" + seg[i + len(words[-1]):] + s[m.end():]
open(p, "w", encoding="utf-8").write(s)
EOF
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"[$qid] quote no longer appears in $qdoc"* ]]; then
  ok "rewording the quoted sentence in $qdoc fails [$qid]"
else bad "reworded quote should fail for $qid: $out"; fi

t="$(fresh_tree bad-fact)"
edit_ledger "$t" 'd["claims"][0]["fact"] = "no-such-fact"'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *'fact "no-such-fact" is not in scripts/facts.json'* ]]; then ok "an unknown fact id fails"; else bad "unknown fact should fail: $out"; fi

t="$(fresh_tree bad-kind)"
edit_ledger "$t" 'd["claims"][0]["kind"] = "vibes"'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *'kind "vibes" is not one of'* ]]; then ok "an unknown kind fails"; else bad "unknown kind should fail: $out"; fi

t="$(fresh_tree dup-id)"
edit_ledger "$t" 'd["claims"].append(dict(d["claims"][0]))'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"duplicate id"* ]]; then ok "a duplicate id fails"; else bad "duplicate id should fail: $out"; fi

t="$(fresh_tree empty)"
edit_ledger "$t" 'd["claims"] = []'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"no claims declared"* ]]; then ok "an empty ledger fails"; else bad "empty ledger should fail: $out"; fi

t="$(fresh_tree orphan-doc)"
edit_ledger "$t" 'doc = d["claims"][0]["doc"]; d["claims"] = [c for c in d["claims"] if c["doc"] != doc]'
run "$t"
if [ "$rc" -ne 0 ] && [[ "$out" == *"has no claims"* ]]; then ok "an audited doc with no claims left fails"; else bad "orphaned audited doc should fail: $out"; fi

echo "check-learn-sync.mjs:"

t="$(fresh_tree learn-pristine)"
run "$t" check-learn-sync.mjs
if [ "$rc" -eq 0 ]; then ok "the pristine /learn copies agree"; else bad "pristine /learn should pass: $out"; fi

t="$(fresh_tree learn-vtt)"
python3 - "$t/website/learn/media/recovery.vtt" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read().replace("Secret Key", "Secret Phrase", 1)
open(p, "w").write(s)
EOF
run "$t" check-learn-sync.mjs
if [ "$rc" -ne 0 ] && [[ "$out" == *"recovery.vtt: beat "*"differs"* ]]; then ok "a caption cue that drifts from its script fails"; else bad "drifted caption should fail: $out"; fi

t="$(fresh_tree learn-script)"
python3 - "$t/learn/manim/scripts/recovery.md" <<'EOF'
import sys
p = sys.argv[1]
s = open(p).read().replace("Two keys, two jobs.", "Two keys, two different jobs.", 1)
open(p, "w").write(s)
EOF
run "$t" check-learn-sync.mjs
if [ "$rc" -ne 0 ] && [[ "$out" == *"recovery.vtt: beat 1 differs"* ]] && [[ "$out" == *"#recovery-transcript-body: beat 1 differs"* ]]; then
  ok "a script edit not copied out fails for both the captions and the transcript"
else bad "unsynced script edit should fail twice: $out"; fi
node "$t/scripts/check-learn-sync.mjs" --root "$t" --write >/dev/null 2>&1
run "$t" check-learn-sync.mjs
if [ "$rc" -eq 0 ] && grep -q "Two keys, two different jobs." "$t/website/learn/media/recovery.vtt" \
   && grep -q "Two keys, two different jobs." "$t/website/learn.html"; then
  ok "--write copies the script into the captions and the transcript"
else bad "--write should resync: $out"; fi

echo
echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
