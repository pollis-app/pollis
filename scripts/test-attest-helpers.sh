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
echo "toolchain recipe — real values, never 'unknown' (#877)"

# 258 permanent leaves across v1.3.4 → v1.9.7 recorded
# `{"rustc":"unknown","node":"unknown","pnpm":"unknown"}` because the attest
# script DEFAULTED the recipe when nothing set it. The defect survived 26
# releases precisely because a wrong recipe and a right one produced identical
# green runs. These assertions encode the rule that replaced it: a recipe is read
# from the build job's manifest or the release fails — there is no third outcome.

printf '{"rustc":"1.96.0","node":"20.19.5","pnpm":"10.14.0","runner_image":"ubuntu22@20250804.1.0"}\n' \
  > "$work/tc-ok.json"
check "reads rustc"        "$(read_toolchain_field "$work/tc-ok.json" rustc)"        "1.96.0"
check "reads node"         "$(read_toolchain_field "$work/tc-ok.json" node)"         "20.19.5"
check "reads pnpm"         "$(read_toolchain_field "$work/tc-ok.json" pnpm)"         "10.14.0"
check "reads runner_image" "$(read_toolchain_field "$work/tc-ok.json" runner_image)" "ubuntu22@20250804.1.0"

# The Linux payload embeds a helper compiled on a DIFFERENT runner image, so the
# recipe has to be able to name both or it sends a rebuilder to the wrong image.
printf '{"rustc":"1.96.0","node":"20.19.5","pnpm":"10.14.0","runner_image":"ubuntu22@20250804.1.0+helper:ubuntu24@20250804.1.0"}\n' \
  > "$work/tc-helper.json"
check "accepts the two-image Linux recipe" \
  "$(read_toolchain_field "$work/tc-helper.json" runner_image)" \
  "ubuntu22@20250804.1.0+helper:ubuntu24@20250804.1.0"

# THE regression guard. The exact string 26 releases published must never be
# accepted, even if a manifest is hand-written containing it.
printf '{"rustc":"unknown","node":"unknown","pnpm":"unknown","runner_image":"ubuntu-22.04"}\n' \
  > "$work/tc-unknown.json"
( read_toolchain_field "$work/tc-unknown.json" rustc ) >/dev/null 2>&1
check "rejects the literal 'unknown' rustc" "$?" "1"
( read_toolchain_field "$work/tc-unknown.json" runner_image ) >/dev/null 2>&1
check "rejects a bare floating runner label" "$?" "1"

# A partially-written manifest must fail per field, not silently yield empty —
# attest-binaries.sh treats an empty read as a hard failure.
printf '{"rustc":"1.96.0","node":"20.19.5"}\n' > "$work/tc-partial.json"
( read_toolchain_field "$work/tc-partial.json" pnpm ) >/dev/null 2>&1
check "rejects a manifest missing a field" "$?" "1"

printf '{"rustc":"stable","node":"20.19.5","pnpm":"10.14.0","runner_image":"ubuntu22@1"}\n' \
  > "$work/tc-floating.json"
( read_toolchain_field "$work/tc-floating.json" rustc ) >/dev/null 2>&1
check "rejects a non-version rustc ('stable' installs no particular compiler)" "$?" "1"

( read_toolchain_field "$work/tc-does-not-exist.json" rustc ) >/dev/null 2>&1
check "returns non-zero for a missing manifest" "$?" "1"
( read_toolchain_field "" rustc ) >/dev/null 2>&1
check "returns non-zero for an empty path argument" "$?" "1"

# runner_image_id: the field the audit called out as "a floating label, not a
# pinned digest". It must refuse to invent one off a hosted runner.
( unset ImageOS ImageVersion; runner_image_id ) >/dev/null 2>&1
check "runner_image_id fails when the runner does not identify itself" "$?" "1"
got="$( ImageOS=ubuntu22 ImageVersion=20250804.1.0 runner_image_id )"
check "runner_image_id names one dated image build" "$got" "ubuntu22@20250804.1.0"

# tool_version: parse and validate, never guess.
if command -v bash >/dev/null 2>&1; then
  fakebin="$work/faketools"; mkdir -p "$fakebin"
  printf '#!/bin/sh\necho "rustc 1.96.0 (abc123 2026-01-01)"\n' > "$fakebin/rustc"
  printf '#!/bin/sh\necho "v20.19.5"\n' > "$fakebin/node"
  printf '#!/bin/sh\necho "10.14.0"\n' > "$fakebin/pnpm"
  printf '#!/bin/sh\necho "some tool, stable channel"\n' > "$fakebin/weird"
  chmod +x "$fakebin"/*
  check "parses rustc's version line" "$(PATH="$fakebin:$PATH" tool_version rustc)" "1.96.0"
  check "strips node's leading v"     "$(PATH="$fakebin:$PATH" tool_version node)"  "20.19.5"
  check "takes pnpm's bare version"   "$(PATH="$fakebin:$PATH" tool_version pnpm)"  "10.14.0"
  ( PATH="$fakebin:$PATH" tool_version weird ) >/dev/null 2>&1
  check "fails when no version can be parsed" "$?" "1"
  out="$( tool_version definitely-not-a-real-compiler 2>&1 )"; rc=$?
  check "fails for a tool that is not installed" "$rc" "1"
  case "$out" in
    *"::error::"*) ok "annotates the missing-tool failure for CI" ;;
    *) bad "missing-tool failure is not annotated: $out" ;;
  esac

  # End-to-end: the manifest the build job writes must be readable by the attest
  # job. Producer and consumer are the pair that actually has to agree.
  ( PATH="$fakebin:$PATH" ImageOS=ubuntu22 ImageVersion=20250804.1.0 \
      write_toolchain_manifest "$work/tc-written.json" "ubuntu24@20250804.1.0" ) >/dev/null 2>&1
  check "write_toolchain_manifest round-trips rustc" \
    "$(read_toolchain_field "$work/tc-written.json" rustc)" "1.96.0"
  check "write_toolchain_manifest round-trips the two-image runner id" \
    "$(read_toolchain_field "$work/tc-written.json" runner_image)" \
    "ubuntu22@20250804.1.0+helper:ubuntu24@20250804.1.0"

  # A build job missing a tool must not produce a manifest at all — a
  # half-written one would be read as a valid recipe for the fields it does have.
  ( PATH="$fakebin:$PATH" ImageOS= ImageVersion= \
      write_toolchain_manifest "$work/tc-nope.json" ) >/dev/null 2>&1
  check "write_toolchain_manifest fails without a runner identity" "$?" "1"
fi

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
echo "strip-pe-signature — normalizing Authenticode out of the Windows payload (#750)"

# The Windows payload digest is taken over the tree inside the SHIPPED, signed
# NSIS installer, so every unreproducible byte Authenticode wrote has to come back
# out first — the certificate table (which embeds an RFC-3161 timestamp that
# differs on every signing) and the PE checksum signtool recomputes over it.
#
# The load-bearing property is the second check below: two DIFFERENT signings of
# the same build must normalize to the same bytes. If they do not, the payload
# leaf is unrecomputable and the whole verifiable-build claim is hollow.
#
# Exercised against synthetic PE images — no real installer, no signtool.
if ! command -v python3 >/dev/null 2>&1; then
  echo "  skip — strip-pe-signature needs python3; covered in CI"
else
  pescript="$here/lib/strip-pe-signature.py"
  peroot="$work/pe"
  mkdir -p "$peroot"

  # Fabricate the fixtures: minimal PE32+ images. A "signed" one has a non-zero
  # checksum and a certificate table appended and pointed to by data directory
  # entry 4, exactly as signtool leaves it.
  python3 - "$peroot" <<'MKPE'
import struct, sys
from pathlib import Path

root = Path(sys.argv[1])

def mkpe(body: bytes, cert: bytes | None) -> bytes:
    dos = bytearray(64)
    dos[0:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 64)
    coff = bytearray(20)
    opt = bytearray(240)
    struct.pack_into("<H", opt, 0, 0x20B)          # PE32+
    struct.pack_into("<I", opt, 108, 16)           # NumberOfRvaAndSizes
    data = bytearray(bytes(dos) + b"PE\0\0" + bytes(coff) + bytes(opt) + body)
    if cert is not None:
        struct.pack_into("<I", data, 88 + 64, 0xDEADBEEF)        # CheckSum
        # signtool aligns the file to 8 bytes before appending the certificate
        # table. Emulated so the padding case is actually exercised below.
        data += b"\0" * (-len(data) % 8)
        off = len(data)
        # Security data directory: optional header at 88, its data directories
        # at +112 (PE32+), entry 4 at +32 from there.
        struct.pack_into("<II", data, 88 + 112 + 32, off, len(cert))
        data += cert
    return bytes(data)

# Length chosen so the image is already 8-byte aligned: signtool adds no
# padding, and normalizing recovers the unsigned bytes exactly.
body = b"compiled application bytes" * 8
(root / "unsigned.exe").write_bytes(mkpe(body, None))
# Two signings of the SAME build: different timestamps, different lengths.
(root / "signed-a.exe").write_bytes(mkpe(body, b"CERT-timestamp-2026-08-08T10:00:00Z-aaaa"))
(root / "signed-b.exe").write_bytes(mkpe(body, b"CERT-timestamp-2026-08-09T22:41:07Z-bbbbbbbbbbbb"))
# ...and a build whose length is NOT 8-aligned, so signing inserts padding that
# truncation cannot distinguish from payload. See the assertions below.
pbody = body + b"odd"
(root / "pad-unsigned.exe").write_bytes(mkpe(pbody, None))
(root / "pad-signed-a.exe").write_bytes(mkpe(pbody, b"CERT-aaaa"))
(root / "pad-signed-b.exe").write_bytes(mkpe(pbody, b"CERT-bbbbbbbbbbbbbbbb"))
# Not a PE at all — must be left completely alone.
(root / "resource.dat").write_bytes(b"\x89PNG not a pe image at all")
# Malformed: bytes follow the certificate table, so truncating would lose them.
bad = bytearray(mkpe(body, b"CERT-short"))
bad += b"TRAILING"
(root / "trailing.exe").write_bytes(bytes(bad))
MKPE

  cp "$peroot/resource.dat" "$peroot/resource.dat.orig"

  # A signed image really does look signed before we touch it.
  python3 "$pescript" check "$peroot/signed-a.exe" >/dev/null 2>&1
  check "check flags a signed PE" "$?" "1"
  python3 "$pescript" check "$peroot/unsigned.exe" >/dev/null 2>&1
  check "check passes an unsigned PE" "$?" "0"

  # Refuse to truncate a layout the parser does not understand.
  python3 "$pescript" strip "$peroot/trailing.exe" >/dev/null 2>&1
  check "strip refuses when bytes follow the certificate table" "$?" "1"
  rm -f "$peroot/trailing.exe"

  python3 "$pescript" strip "$peroot" >/dev/null 2>&1
  check "strip succeeds over a mixed tree" "$?" "0"
  python3 "$pescript" check "$peroot" >/dev/null 2>&1
  check "nothing in the tree is signed afterwards" "$?" "0"

  # THE load-bearing assertion: two different signings normalize to identical
  # bytes, so the payload digest does not depend on which signing produced the
  # installer a verifier happens to hold.
  sa="$(sha256sum "$peroot/signed-a.exe" | awk '{print $1}')"
  sb="$(sha256sum "$peroot/signed-b.exe" | awk '{print $1}')"
  check "two different signings normalize to the same bytes" "$sa" "$sb"

  # ...and, when signing needed no alignment padding, to the same bytes as the
  # build that was never signed at all.
  su="$(sha256sum "$peroot/unsigned.exe" | awk '{print $1}')"
  check "an unpadded signed image normalizes to the unsigned build" "$sa" "$su"

  # When the image was NOT 8-byte aligned, signtool inserted padding before the
  # certificate table, and truncation cannot tell that padding from payload. The
  # guarantee that survives is the one that matters: every signing of a given
  # build still normalizes to the SAME bytes, so the digest is recomputable by
  # anyone holding any copy of the installer. Equality with the never-signed
  # build is NOT claimed here, and chasing it by stripping trailing zeros would
  # corrupt any binary that legitimately ends in them.
  pa="$(sha256sum "$peroot/pad-signed-a.exe" | awk '{print $1}')"
  pb="$(sha256sum "$peroot/pad-signed-b.exe" | awk '{print $1}')"
  pu="$(sha256sum "$peroot/pad-unsigned.exe" | awk '{print $1}')"
  check "padded signings still normalize to each other" "$pa" "$pb"
  check "padding is retained, not guessed away" "$( [ "$pa" != "$pu" ] && echo differs )" "differs"

  # Non-PE payload files are untouched — the digest must still cover them.
  do1="$(sha256sum "$peroot/resource.dat" | awk '{print $1}')"
  do2="$(sha256sum "$peroot/resource.dat.orig" | awk '{print $1}')"
  check "non-PE files are left byte-identical" "$do1" "$do2"

  # Idempotent: normalizing an already-normalized tree changes nothing.
  before="$(cd "$peroot" && sha256sum ./*.exe | sha256sum)"
  python3 "$pescript" strip "$peroot" >/dev/null 2>&1
  after="$(cd "$peroot" && sha256sum ./*.exe | sha256sum)"
  check "strip is idempotent" "$before" "$after"

  # The interpreter resolver. Git-for-Windows bash does not reliably expose
  # `python3`, and stock Windows ships a `python3.exe` Store alias that runs
  # nothing — so the resolver must PROVE a candidate works, not just find it on
  # PATH. Without this the Windows release would fail only at the very end of the
  # job, after a ~30-minute signed build.
  ( . "$here/lib/payload-hash.sh" && payload_hash_python >/dev/null ) 2>/dev/null
  check "resolves a working interpreter" "$?" "0"

  got="$( . "$here/lib/payload-hash.sh" && PYTHON=python3 payload_hash_python 2>/dev/null )"
  check "honours a \$PYTHON override" "$got" "python3"

  # A named interpreter that does not exist must fall back, not fail.
  got="$( . "$here/lib/payload-hash.sh" && PYTHON=definitely-not-an-interpreter payload_hash_python 2>/dev/null )"
  check "falls back when \$PYTHON does not exist" "$got" "python3"

  # A candidate that EXISTS but is not Python 3 must be rejected — this is the
  # Store-alias case, which is on PATH and does nothing useful.
  fakebin="$work/fakepy"; mkdir -p "$fakebin"
  printf '#!/bin/sh\nexit 9\n' > "$fakebin/python3"
  chmod +x "$fakebin/python3"
  got="$( . "$here/lib/payload-hash.sh" && PATH="$fakebin:$PATH" payload_hash_python 2>/dev/null )"
  check "rejects a non-working interpreter on PATH" "$got" "python"

  # Nothing usable at all is a loud failure, not a silent empty string.
  out="$( . "$here/lib/payload-hash.sh" && PATH="$fakebin" PYTHON= payload_hash_python 2>&1 )"; rc=$?
  check "fails when no interpreter works" "$rc" "1"
  case "$out" in
    *"no working Python 3 interpreter"*) ok "names the problem" ;;
    *) bad "unhelpful resolver error: $out" ;;
  esac
fi

echo "sha_windows_payload — the shipped-installer payload digest (#750)"

# End-to-end over the real function, not just the stripper: extract an archive,
# drop the NSIS scaffolding, normalize every PE, hash the tree. A .7z stands in
# for the NSIS installer — sha_windows_payload only ever runs `7z x` on it, so the
# container format is irrelevant to what is being tested.
#
# THE assertion is the third one: two independently signed builds of the same
# source must produce the SAME payload digest. That is the whole claim the leaf
# makes, and it is what the two-build scheme could not deliver.
if ! command -v 7z >/dev/null 2>&1 || ! command -v python3 >/dev/null 2>&1; then
  echo "  skip — sha_windows_payload needs 7z + python3; covered in CI"
else
  . "$here/lib/payload-hash.sh"
  export SOURCE_DATE_EPOCH=1700000000
  wroot="$work/winpay"
  mkdir -p "$wroot"

  # Two "releases" of the same build, signed differently, plus scaffolding NSIS
  # adds that is not installed payload and must not enter the digest.
  for v in a b; do
    d="$wroot/tree-$v/\$PLUGINSDIR"
    mkdir -p "$d"
    cp "$peroot/signed-$v.exe" "$wroot/tree-$v/pollis.exe" 2>/dev/null || true
    printf 'resource payload' > "$wroot/tree-$v/resources.dat"
    printf 'installer plugin %s' "$v" > "$d/System.dll"
    printf 'uninstaller %s' "$v" > "$wroot/tree-$v/Uninstall.exe"
    ( cd "$wroot/tree-$v" && 7z a -bso0 -bsp0 "../installer-$v.7z" . >/dev/null )
  done

  # signed-a/signed-b were already normalized in place by the block above, so
  # re-sign them here to get genuinely different signed inputs.
  python3 - "$wroot" <<'MKSIGNED'
import struct, sys, subprocess, shutil
from pathlib import Path
root = Path(sys.argv[1])

def mkpe(body: bytes, cert: bytes | None) -> bytes:
    dos = bytearray(64); dos[0:2] = b"MZ"
    struct.pack_into("<I", dos, 0x3C, 64)
    opt = bytearray(240)
    struct.pack_into("<H", opt, 0, 0x20B)
    struct.pack_into("<I", opt, 108, 16)
    data = bytearray(bytes(dos) + b"PE\0\0" + bytes(bytearray(20)) + bytes(opt) + body)
    if cert is not None:
        struct.pack_into("<I", data, 88 + 64, 0xDEADBEEF)
        data += b"\0" * (-len(data) % 8)
        struct.pack_into("<II", data, 88 + 112 + 32, len(data), len(cert))
        data += cert
    return bytes(data)

body = b"identical compiled bytes" * 16
for v, cert in (("a", b"CERT-signing-one"), ("b", b"CERT-signing-two-much-longer")):
    (root / f"tree-{v}" / "pollis.exe").write_bytes(mkpe(body, cert))
MKSIGNED

  for v in a b; do
    rm -f "$wroot/installer-$v.7z"
    ( cd "$wroot/tree-$v" && 7z a -bso0 -bsp0 "../installer-$v.7z" . >/dev/null )
  done

  da="$(sha_windows_payload "$wroot/installer-a.7z" 2>/dev/null)"
  check "produces a digest for a signed installer" "${#da}" "64"

  db="$(sha_windows_payload "$wroot/installer-b.7z" 2>/dev/null)"
  check "two independently signed builds agree" "$da" "$db"

  # Deterministic across runs.
  da2="$(sha_windows_payload "$wroot/installer-a.7z" 2>/dev/null)"
  check "the digest is stable across invocations" "$da" "$da2"

  # A real payload change MUST move the digest, or the leaf proves nothing.
  printf 'DIFFERENT resource' > "$wroot/tree-a/resources.dat"
  rm -f "$wroot/installer-a.7z"
  ( cd "$wroot/tree-a" && 7z a -bso0 -bsp0 "../installer-a.7z" . >/dev/null )
  dc="$(sha_windows_payload "$wroot/installer-a.7z" 2>/dev/null)"
  check "a changed payload changes the digest" "$( [ "$dc" != "$da" ] && echo differs )" "differs"

  sha_windows_payload "$wroot/no-such-installer.exe" >/dev/null 2>&1
  check "a missing installer fails loudly" "$?" "1"

  ( unset SOURCE_DATE_EPOCH; sha_windows_payload "$wroot/installer-b.7z" ) >/dev/null 2>&1
  check "refuses to run without SOURCE_DATE_EPOCH" "$?" "1"
  unset SOURCE_DATE_EPOCH
fi

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

# windows: the flags must reach signtool LITERALLY.
#
# Git-for-Windows bash rewrites Unix-looking arguments before calling a native binary,
# which turned `/pa` into `C:/Program Files/Git/pa` and `/v` into `V:/` and failed a
# real release for a correctly signed installer. MSYS2_ARG_CONV_EXCL suppresses that.
#
# This box is Linux, where no such rewriting happens, so the argv assertion alone would
# pass with or without the fix and protect nothing. The load-bearing assertion is the
# second one: that signtool is invoked with the exclusion actually set. Remove the env
# var and that fails here, on Linux, instead of on the next release.
stub="$work/signtool-stub.sh"
cat > "$stub" <<'STUB'
#!/usr/bin/env bash
printf 'argv:%s\n' "$*"
printf 'excl:%s\n' "${MSYS2_ARG_CONV_EXCL:-<unset>}"
STUB
chmod +x "$stub"
printf 'pe' > "$work/signed.exe"
out="$( "$sscript" windows "$work/signed.exe" "$stub" 2>&1 )"
case "$out" in
  *"argv:verify /pa /v $work/signed.exe"*) ok "signtool receives verify /pa /v <exe>" ;;
  *) bad "unexpected signtool argv: $out" ;;
esac
case "$out" in
  *"excl:"*"/pa"*) ok "the /pa flag is excluded from MSYS path conversion" ;;
  *) bad "MSYS2_ARG_CONV_EXCL does not cover /pa — Git bash will mangle it: $out" ;;
esac
case "$out" in
  *"excl:"*"/v"*) ok "the /v flag is excluded from MSYS path conversion" ;;
  *) bad "MSYS2_ARG_CONV_EXCL does not cover /v — Git bash will mangle it: $out" ;;
esac

# windows: a non-zero signtool exit is a hard failure — an unsigned artifact must never
# reach the upload.
failstub="$work/signtool-fail.sh"
printf '#!/usr/bin/env bash\nexit 1\n' > "$failstub"
chmod +x "$failstub"
out="$( "$sscript" windows "$work/signed.exe" "$failstub" 2>&1 )"; rc=$?
check "windows fails when signtool rejects the signature" "$rc" "1"
case "$out" in
  *"not"*"validly Authenticode-signed"*) ok "rejection explains the consequence" ;;
  *) bad "signtool rejection error is unclear: $out" ;;
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
