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
echo "read_payload_sha256 — the pre-signature payload sidecar contract (#603/#704)"

# The macOS/Windows payload leaf is the build job's pre-signature sha_tree,
# carried to the attest job in a tiny sidecar. The reader must accept a valid
# 64-hex digest (with or without a trailing newline) and reject everything else,
# so a malformed/missing sidecar hard-fails loudly instead of logging a garbage
# payload hash.
valid="$(printf 'a%.0s' $(seq 1 64))"
printf '%s\n' "$valid" > "$work/ok.sha"
check "accepts a 64-hex digest (trailing newline tolerated)" "$(read_payload_sha256 "$work/ok.sha")" "$valid"

printf '  %s  \n' "$valid" > "$work/ws.sha"
check "strips surrounding whitespace" "$(read_payload_sha256 "$work/ws.sha")" "$valid"

printf '%s' "$valid" > "$work/nonl.sha"
check "accepts a digest with no trailing newline" "$(read_payload_sha256 "$work/nonl.sha")" "$valid"

# Uppercase is not what sha256sum emits (lowercase hex) — reject, don't silently
# lowercase, so a mismatched producer is caught rather than papered over.
printf '%s\n' "$(printf 'A%.0s' $(seq 1 64))" > "$work/upper.sha"
( read_payload_sha256 "$work/upper.sha" ) >/dev/null 2>&1
check "rejects uppercase hex" "$?" "1"

printf '%s\n' "$(printf 'a%.0s' $(seq 1 63))" > "$work/short.sha"
( read_payload_sha256 "$work/short.sha" ) >/dev/null 2>&1
check "rejects a too-short digest" "$?" "1"

printf 'not-a-hash zzz\n' > "$work/junk.sha"
( read_payload_sha256 "$work/junk.sha" ) >/dev/null 2>&1
check "rejects non-hex content" "$?" "1"

( read_payload_sha256 "$work/does-not-exist.sha" ) >/dev/null 2>&1
check "returns non-zero for a missing manifest" "$?" "1"

( read_payload_sha256 "" ) >/dev/null 2>&1
check "returns non-zero for an empty path argument" "$?" "1"

# The macOS capture runs on a BSD-tar host and sets TAR=gtar; sha_tree must honour
# the override and produce the SAME bytes as the default GNU `tar` everywhere else,
# or the logged and rebuilt hashes would diverge on an incidental tool difference.
if command -v sha256sum >/dev/null 2>&1 && { command -v gtar >/dev/null 2>&1 || tar --version 2>/dev/null | grep -qi 'gnu tar'; }; then
  export SOURCE_DATE_EPOCH=1700000000
  mkdir -p "$work/tar1/sub"; echo x > "$work/tar1/sub/f"; echo y > "$work/tar1/g"
  default_hash="$(sha_tree "$work/tar1")"
  check "sha_tree honours TAR override (TAR=tar == default)" "$(TAR=tar sha_tree "$work/tar1")" "$default_hash"
  unset SOURCE_DATE_EPOCH
else
  echo "  skip — TAR-override check needs GNU tar + sha256sum; covered in CI"
fi

echo
echo "verify-presig-binary — the WS3 pre-signature/shipped-binary invariant (#603/#704)"

# The WINDOWS payload leaf hashes an UNSIGNED capture build; the signed build
# recompiles the app crate (a config delta trips tauri-build's rerun-if-changed).
# This script records the compiled binary's fingerprint after the capture build and
# re-checks it after the signed build, so a divergence stops the release rather than
# publishing a payload leaf for a binary that never shipped. Exercised here against
# a fabricated target/ tree — no real release.
#
# macOS no longer uses this path at all: since #750 it builds ONCE and derives its
# payload digest from the shipped bundle (sha_macos_payload), so there are never two
# compiles to reconcile. Windows still builds twice and so still needs the check.
if ! command -v sha256sum >/dev/null 2>&1; then
  echo "  skip — verify-presig-binary needs sha256sum (absent on macOS); covered in CI"
else
  vscript="$here/verify-presig-binary.sh"
  # Linux triple: main binary only (no macOS externalBin sidecar).
  ltriple="x86_64-unknown-linux-gnu"
  vroot="$work/vpb-linux"
  mkdir -p "$vroot/target/$ltriple/release"
  printf 'compiled-v1' > "$vroot/target/$ltriple/release/pollis"
  ( cd "$vroot" && "$vscript" record "$ltriple" fp.txt ) >/dev/null 2>&1
  check "record writes a fingerprint file" "$( [ -s "$vroot/fp.txt" ] && echo yes )" "yes"
  ( cd "$vroot" && "$vscript" verify "$ltriple" fp.txt ) >/dev/null 2>&1
  check "verify passes when the binary is unchanged" "$?" "0"

  printf 'compiled-v2-DIFFERENT' > "$vroot/target/$ltriple/release/pollis"
  out="$( cd "$vroot" && "$vscript" verify "$ltriple" fp.txt 2>&1 )"; rc=$?
  check "verify fails when the compiled binary changed" "$rc" "1"
  case "$out" in
    *"WS3 INVARIANT VIOLATED"*) ok "mismatch explains the meaning, not a bare diff" ;;
    *) bad "mismatch error lacks the WS3 explanation: $out" ;;
  esac

  ( cd "$vroot" && "$vscript" verify "$ltriple" no-such-file.txt ) >/dev/null 2>&1
  check "verify fails when no fingerprint was recorded" "$?" "1"

  ( cd "$vroot" && "$vscript" record "aarch64-unknown-none" x.txt ) >/dev/null 2>&1
  check "record fails when the compiled binary is absent" "$?" "1"

  # Windows triple: the main binary is <name>.exe, and there is no capture sidecar.
  wtriple="x86_64-pc-windows-msvc"
  vwin="$work/vpb-win"
  mkdir -p "$vwin/target/$wtriple/release"
  printf 'pe-bytes' > "$vwin/target/$wtriple/release/pollis.exe"
  ( cd "$vwin" && "$vscript" record "$wtriple" fp.txt ) >/dev/null 2>&1
  check "record resolves the Windows .exe binary" "$?" "0"

  # macOS triple: the externalBin capture-helper sidecar is part of the payload,
  # so a change to it alone must also fail the invariant.
  mtriple="aarch64-apple-darwin"
  vmac="$work/vpb-mac"
  mkdir -p "$vmac/target/$mtriple/release" "$vmac/src-tauri/binaries"
  printf 'macho' > "$vmac/target/$mtriple/release/pollis"
  printf 'helper-v1' > "$vmac/src-tauri/binaries/pollis-capture-macos-$mtriple"
  ( cd "$vmac" && "$vscript" record "$mtriple" fp.txt ) >/dev/null 2>&1
  lines="$(wc -l < "$vmac/fp.txt" | tr -d ' ')"
  check "macOS fingerprint covers binary + capture-helper sidecar" "$lines" "2"
  printf 'helper-v2-DIFFERENT' > "$vmac/src-tauri/binaries/pollis-capture-macos-$mtriple"
  ( cd "$vmac" && "$vscript" verify "$mtriple" fp.txt ) >/dev/null 2>&1
  check "verify catches a changed sidecar (not just the main binary)" "$?" "1"
fi

echo
echo "verify-signed-artifact — the WS3 'the shipped artifact is signed' gate (#603/#704)"

# WS3 removed the hardcoded macOS signingIdentity from the config, so the whole
# signing config lives in secrets. GitHub sets an unset secret's env var to the
# EMPTY STRING (Some("") not None), so an empty identity would SILENTLY skip
# signing + notarization and still go green. The `preflight` mode turns that into
# a loud failure; it is pure bash and fully exercised here. The `macos`/`windows`
# modes shell out to codesign/spctl/signtool, which exist only on their runners —
# so those are code-reviewed, and here we cover their pre-tool guards + dispatch.
sscript="$here/verify-signed-artifact.sh"

# preflight: all present → pass.
export SIG_A=identity SIG_B=cert
( "$sscript" preflight "macOS" SIG_A SIG_B ) >/dev/null 2>&1
check "preflight passes when every named secret is non-empty" "$?" "0"

# preflight: one empty (the Some("") GitHub-Actions case) → fail, and the error
# must name the empty var and say why, not just exit non-zero.
export SIG_EMPTY=""
out="$( "$sscript" preflight "macOS" SIG_A SIG_EMPTY 2>&1 )"; rc=$?
check "preflight fails when a secret is the empty string" "$rc" "1"
case "$out" in
  *"SIG_EMPTY is empty or unset"*) ok "names the empty secret in the error" ;;
  *) bad "error does not name the empty secret: $out" ;;
esac
case "$out" in
  *"must not be published"*) ok "explains the unsigned-artifact hazard, not a bare code" ;;
  *) bad "error lacks the WS3 unsigned-artifact explanation: $out" ;;
esac

# preflight: an UNSET var (never exported) is treated identically to empty — the
# indirect expansion must not trip `set -u` and must count as a failure.
unset SIG_NEVER_SET 2>/dev/null || true
out="$( "$sscript" preflight "Windows" SIG_NEVER_SET 2>&1 )"; rc=$?
check "preflight fails (not crashes) on a fully unset secret" "$rc" "1"
case "$out" in
  *"SIG_NEVER_SET is empty or unset"*) ok "treats unset the same as empty" ;;
  *) bad "unset var not reported as empty/unset: $out" ;;
esac

# preflight: multiple empties are all reported, not just the first — so one CI run
# names every missing secret instead of a whack-a-mole of re-runs.
export SIG_E1="" SIG_E2=""
out="$( "$sscript" preflight "macOS" SIG_E1 SIG_A SIG_E2 2>&1 )"; rc=$?
check "preflight fails when several secrets are empty" "$rc" "1"
if grep -q "SIG_E1 is empty" <<<"$out" && grep -q "SIG_E2 is empty" <<<"$out"; then
  ok "reports every empty secret in one pass"
else
  bad "did not report all empty secrets: $out"
fi

# preflight: usage errors are exit 2 (a caller mistake), distinct from exit 1
# (a real empty-secret failure).
( "$sscript" preflight "macOS" ) >/dev/null 2>&1
check "preflight with no var names is a usage error (exit 2)" "$?" "2"

# Unknown mode → usage error, never a silent success.
( "$sscript" bogus-mode arg ) >/dev/null 2>&1
check "unknown mode is a usage error (exit 2)" "$?" "2"

# macos: a missing .app bundle fails loudly BEFORE reaching codesign, so the guard
# is what a Linux box (no codesign) can assert.
out="$( "$sscript" macos "$work/no-such.app" 2>&1 )"; rc=$?
check "macos fails when the .app bundle is absent" "$rc" "1"
case "$out" in
  *".app bundle not found"*) ok "macos names the missing bundle" ;;
  *) bad "macos error does not name the missing bundle: $out" ;;
esac

# windows: a missing installer .exe fails loudly before reaching signtool.
out="$( "$sscript" windows "$work/no-such.exe" 2>&1 )"; rc=$?
check "windows fails when the installer .exe is absent" "$rc" "1"
case "$out" in
  *".exe not found"*) ok "windows names the missing installer" ;;
  *) bad "windows error does not name the missing installer: $out" ;;
esac

# windows: with a real .exe present but no signtool resolvable, the signtool guard
# fires (rather than a confusing failure deep in an invocation).
printf 'pe' > "$work/present.exe"
out="$( SIGNTOOL_PATH="" "$sscript" windows "$work/present.exe" 2>&1 )"; rc=$?
check "windows fails when signtool cannot be resolved" "$rc" "1"
case "$out" in
  *"signtool not found"*) ok "windows names the missing signtool" ;;
  *) bad "windows error does not name the missing signtool: $out" ;;
esac
unset SIG_A SIG_B SIG_EMPTY SIG_E1 SIG_E2 2>/dev/null || true

echo
echo "sha_macos_payload — the shipped-bundle payload digest (#750)"

# The macOS payload digest is derived from the SIGNED bundle we ship, normalizing
# Apple's per-signing material back out. The property that has to hold is exact:
# stripping must remove the signature and NOTHING ELSE, so the digest of a signed
# bundle equals the digest that same build would have had unsigned — and two
# separate signings of one build agree.
#
# codesign/stapler exist only on macOS runners, so they are stubbed here against a
# fabricated bundle. The stubs model the shape the real tools present: an embedded
# per-Mach-O signature (a trailing __SIG__ line), a stapled ticket file, and a
# _CodeSignature manifest directory. That is enough to exercise every branch,
# including the two fail-loud guards, which is the part that must never regress:
# a silently-failing strip would publish a digest nobody could ever recompute.
stub="$work/stub-bin"; mkdir -p "$stub"

cat > "$stub/codesign" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  --remove-signature)
    shift
    for f in "$@"; do
      [ -f "$f" ] || continue
      grep -v '^__SIG__' "$f" > "$f.stripped" || true
      mv "$f.stripped" "$f"
    done
    ;;
  -dv|-d|--verify)
    shift
    grep -rq '^__SIG__' "${1:-}" 2>/dev/null && exit 0
    exit 1
    ;;
esac
exit 0
STUB

cat > "$stub/xcrun" <<'STUB'
#!/usr/bin/env bash
if [ "${1:-}" = "stapler" ]; then
  case "${2:-}" in
    unstaple) rm -f "${3:-}/Contents/_Ticket"; exit 0 ;;
    validate) [ -f "${3:-}/Contents/_Ticket" ] && exit 0; exit 1 ;;
  esac
fi
exit 0
STUB

# `file -b <path>` — Mach-O iff the payload starts with our MACHO marker.
cat > "$stub/file" <<'STUB'
#!/usr/bin/env bash
f="${2:-}"
if [ -f "$f" ] && head -c 5 "$f" 2>/dev/null | grep -q 'MACHO'; then
  echo "Mach-O 64-bit executable arm64"
else
  echo "ASCII text"
fi
exit 0
STUB
chmod +x "$stub"/*

# build_app <dir> <signed:yes|no> [ticket-bytes]
build_app() {
  local d="$1" signed="$2" ticket="${3:-ticket-bytes}"
  mkdir -p "$d/Contents/MacOS" "$d/Contents/Resources"
  printf 'MACHO\nprogram-bytes\n' > "$d/Contents/MacOS/pollis"
  printf 'MACHO\nhelper-bytes\n'  > "$d/Contents/MacOS/pollis-capture-macos"
  printf 'console.log(1)\n'       > "$d/Contents/Resources/app.js"
  if [ "$signed" = "yes" ]; then
    printf '__SIG__cms-blob-a\n' >> "$d/Contents/MacOS/pollis"
    printf '__SIG__cms-blob-b\n' >> "$d/Contents/MacOS/pollis-capture-macos"
    mkdir -p "$d/Contents/_CodeSignature"
    printf '<plist>resources</plist>\n' > "$d/Contents/_CodeSignature/CodeResources"
    printf '%s\n' "$ticket" > "$d/Contents/_Ticket"
  fi
}

if ! command -v sha256sum >/dev/null 2>&1; then
  echo "  skip — sha_macos_payload tests need sha256sum; covered in CI"
else
  mroot="$work/macos"; mkdir -p "$mroot"
  build_app "$mroot/Signed.app"   yes "ticket-from-submission-1"
  build_app "$mroot/Signed2.app"  yes "ticket-from-submission-2"
  build_app "$mroot/Unsigned.app" no

  export SOURCE_DATE_EPOCH=1700000000
  unsigned_digest="$(sha_tree "$mroot/Unsigned.app")"

  sig_digest="$(PATH="$stub:$PATH" sha_macos_payload "$mroot/Signed.app")"; rc=$?
  check "normalizing a signed bundle succeeds" "$rc" "0"

  # THE load-bearing assertion. If these ever diverge, the digest is a function of
  # the signing operation rather than of the build, and the leaf becomes something
  # no outside party can reproduce from the public .dmg.
  check "signed bundle normalizes to the SAME digest as the unsigned build" \
    "$sig_digest" "$unsigned_digest"

  sig2_digest="$(PATH="$stub:$PATH" sha_macos_payload "$mroot/Signed2.app")"
  check "a second, independent signing of the same build agrees" \
    "$sig2_digest" "$sig_digest"

  # Normalization must not touch the bundle we ship — it runs on a copy.
  check "the shipped bundle still carries its signature afterwards" \
    "$(grep -c '^__SIG__' "$mroot/Signed.app/Contents/MacOS/pollis")" "1"
  check "the shipped bundle still carries its notarization ticket" \
    "$( [ -f "$mroot/Signed.app/Contents/_Ticket" ] && echo yes )" "yes"

  # Fail-loud guard 1: codesign silently does nothing (wrong Xcode, SIP refusal,
  # a future flag rename). Hashing anyway would publish an unrecomputable leaf.
  cat > "$stub/codesign" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  -dv|-d|--verify) exit 0 ;;
esac
exit 0
STUB
  chmod +x "$stub/codesign"
  out="$(PATH="$stub:$PATH" sha_macos_payload "$mroot/Signed.app" 2>&1)"; rc=$?
  check "fails when the code signature survives the strip" "$rc" "1"
  case "$out" in
    *"STILL carries a code signature"*) ok "signature-survived error says what went wrong" ;;
    *) bad "signature-survived error is unclear: $out" ;;
  esac

  # Fail-loud guard 2: the ticket survives unstaple.
  cat > "$stub/codesign" <<'STUB'
#!/usr/bin/env bash
case "${1:-}" in
  --remove-signature)
    shift
    for f in "$@"; do
      [ -f "$f" ] || continue
      grep -v '^__SIG__' "$f" > "$f.stripped" || true
      mv "$f.stripped" "$f"
    done
    ;;
  -dv|-d|--verify) exit 1 ;;
esac
exit 0
STUB
  cat > "$stub/xcrun" <<'STUB'
#!/usr/bin/env bash
[ "${1:-}" = "stapler" ] && [ "${2:-}" = "validate" ] && exit 0
exit 0
STUB
  chmod +x "$stub/codesign" "$stub/xcrun"
  out="$(PATH="$stub:$PATH" sha_macos_payload "$mroot/Signed2.app" 2>&1)"; rc=$?
  check "fails when the notarization ticket survives unstaple" "$rc" "1"
  case "$out" in
    *"STILL carries a notarization ticket"*) ok "ticket-survived error says what went wrong" ;;
    *) bad "ticket-survived error is unclear: $out" ;;
  esac

  # A path that is not a bundle fails before touching any Apple tooling.
  ( PATH="$stub:$PATH" sha_macos_payload "$mroot/no-such.app" ) >/dev/null 2>&1
  check "a missing bundle fails loudly" "$?" "1"

  unset SOURCE_DATE_EPOCH
fi

echo
echo "──────────────────────────────────────────"
echo "passed: $pass   failed: $fail"
[ "$fail" -eq 0 ] || exit 1
