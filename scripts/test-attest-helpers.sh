#!/usr/bin/env bash
#
# test-attest-helpers.sh — tests for the attest layer that keeps breaking.
#
# Three consecutive releases were broken by bugs in package-extraction and
# tooling assumptions (#588, #602, #604), every one of which was invisible until
# a real release ran. This exercises that layer directly, plus the payload-hash
# contract the independent rebuilder depends on.
#
# Run: ./scripts/test-attest-helpers.sh   (no arguments, no network, no CI-only tools)
set -uo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
. "$here/lib/attest-helpers.sh"
. "$here/lib/payload-hash.sh"

pass=0; fail=0
ok()   { pass=$((pass+1)); echo "  ok   — $1"; }
bad()  { fail=$((fail+1)); echo "  FAIL — $1"; }
check() { if [ "$2" = "$3" ]; then ok "$1"; else bad "$1 (expected '$3', got '$2')"; fi; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "find_installed_bin — packager prefix independence"

# dpkg-deb --fsys-tarfile emits `usr/bin/pollis`; rpm's cpio emits
# `./usr/bin/pollis`. #588 shipped a hardcoded `./usr/bin/pollis` member path,
# which broke the .deb and took down the v1.5.3 attest.
mkdir -p "$work/deb-style/usr/bin" && echo binary > "$work/deb-style/usr/bin/pollis"
found="$(find_installed_bin "$work/deb-style")"
check "finds the binary in a deb-style tree (no ./ prefix)" "$(basename "${found:-none}")" "pollis"

mkdir -p "$work/rpm-style/./usr/bin" && echo binary > "$work/rpm-style/usr/bin/pollis"
found="$(find_installed_bin "$work/rpm-style")"
check "finds the binary in an rpm-style tree (./ prefix)" "$(basename "${found:-none}")" "pollis"

# A tree with no main binary must yield empty, so emit_exe's guard fires with a
# clear error instead of hashing something arbitrary.
mkdir -p "$work/empty/usr/share" && echo x > "$work/empty/usr/share/thing"
found="$(find_installed_bin "$work/empty")"
check "returns empty when the main binary is absent" "${found:-<empty>}" "<empty>"

# `usr/bin/pollis` must match specifically — not `pollis-capture-linux`, which
# ships alongside it in every Linux package.
mkdir -p "$work/helper/usr/bin" && echo helper > "$work/helper/usr/bin/pollis-capture-linux"
found="$(find_installed_bin "$work/helper")"
check "does not mistake the capture helper for the main binary" "${found:-<empty>}" "<empty>"

# Nested roots (packages extracted into a subdirectory) must still resolve.
mkdir -p "$work/nested/opt/stage/usr/bin" && echo binary > "$work/nested/opt/stage/usr/bin/pollis"
found="$(find_installed_bin "$work/nested")"
check "finds the binary under a nested extraction root" "$(basename "${found:-none}")" "pollis"

echo
echo "need — missing tooling fails loudly"

# The v1.5.3 backfill died with a bare `exit 1` and an empty log. `need` must
# exit non-zero AND say which tool, or that failure mode returns.
out="$(need definitely-not-a-real-tool "do the thing" 2>&1)"; rc=$?
check "exits non-zero for a missing tool" "$rc" "1"
case "$out" in
  *"definitely-not-a-real-tool"*) ok "names the missing tool in the error" ;;
  *) bad "error text does not name the missing tool: $out" ;;
esac
case "$out" in
  *"::error::"*) ok "emits a CI error annotation" ;;
  *) bad "error is not annotated for CI" ;;
esac
( need sh "run a shell" ) >/dev/null 2>&1
check "succeeds for a tool that exists" "$?" "0"

echo
echo "payload-hash — the contract the independent rebuilder recomputes"

# `sha_file` shells out to sha256sum, which does not exist on macOS (it has
# `shasum -a 256`) — and it returns EMPTY rather than failing when absent, so
# every hash would compare equal. Production is protected by the attest script's
# `need sha256sum` guard; here we skip rather than assert a vacuous pass.
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "  skip — sha_file/sha_tree need sha256sum (absent on macOS); covered in CI"
else
echo "content" > "$work/a"; echo "content" > "$work/b"; echo "different" > "$work/c"
check "sha_file is stable for identical content" "$(sha_file "$work/a")" "$(sha_file "$work/b")"
if [ "$(sha_file "$work/a")" != "$(sha_file "$work/c")" ]; then
  ok "sha_file differs for different content"
else
  bad "sha_file collided on different content"
fi

# sha_tree must be a pure function of contents given a fixed SOURCE_DATE_EPOCH —
# that is the whole reason it exists rather than a plain tar hash.
export SOURCE_DATE_EPOCH=1700000000
mkdir -p "$work/t1/sub" "$work/t2/sub"
echo one > "$work/t1/sub/f1"; echo two > "$work/t1/f2"
echo one > "$work/t2/sub/f1"; echo two > "$work/t2/f2"
if command -v gtar >/dev/null 2>&1 || tar --version 2>/dev/null | grep -qi 'gnu tar'; then
  check "sha_tree is identical for identical trees" "$(sha_tree "$work/t1")" "$(sha_tree "$work/t2")"
  # Touching the mtimes must NOT change the hash — SOURCE_DATE_EPOCH pins them.
  touch -t 202001010000 "$work/t2/f2"
  check "sha_tree ignores mtimes (SOURCE_DATE_EPOCH pinned)" "$(sha_tree "$work/t1")" "$(sha_tree "$work/t2")"
  echo changed > "$work/t2/f2"
  if [ "$(sha_tree "$work/t1")" != "$(sha_tree "$work/t2")" ]; then
    ok "sha_tree changes when contents change"
  else
    bad "sha_tree did not change when contents changed"
  fi
else
  echo "  skip — sha_tree needs GNU tar (BSD tar lacks --sort=name); covered in CI"
fi

# sha_tree without SOURCE_DATE_EPOCH must refuse rather than silently hash `now`,
# which would make the logged payload unreproducible.
unset SOURCE_DATE_EPOCH
( sha_tree "$work/t1" ) >/dev/null 2>&1
if [ "$?" -ne 0 ]; then
  ok "sha_tree refuses to run without SOURCE_DATE_EPOCH"
else
  bad "sha_tree ran without SOURCE_DATE_EPOCH — logged payloads would not reproduce"
fi
fi

echo
echo "──────────────────────────────────────────"
echo "passed: $pass   failed: $fail"
[ "$fail" -eq 0 ] || exit 1
