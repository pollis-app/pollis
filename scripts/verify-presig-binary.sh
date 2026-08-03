#!/usr/bin/env bash
#
# verify-presig-binary.sh — the WS3 (#603/#704) load-bearing invariant: PROVE the
# pre-signature payload leaf wraps the SAME compiled binary the signed release
# ships, instead of asserting it in a comment.
#
# The macOS/Windows `payload` leaf is a sha_tree of the UNSIGNED .app / NSIS tree
# captured by a first `tauri build` (the "capture" build), before code signing.
# The shipped installer is then produced by a SECOND `tauri build` (the "signed"
# build). Those two builds do not see a byte-identical Tauri config — the capture
# build disables the updater artifacts it cannot sign, and on Windows the signed
# build injects the Authenticode `signCommand`. tauri-build emits
# `cargo:rerun-if-env-changed=TAURI_CONFIG` and `cargo:rerun-if-changed=tauri.conf.json`,
# so the signed build RECOMPILES the app crate.
#
# Every one of those config deltas lives in a `bundle` field Tauri's codegen
# strips to Default before embedding the config into the binary (tauri-utils
# `BundleConfig::to_tokens` zeroes `macos` and `create_updater_artifacts`;
# `WindowsConfig::to_tokens` drops `sign_command`), so the recompiled binary is
# EXPECTED to be byte-identical. But "expected" is not "checked": a future Tauri
# or config change could silently reintroduce divergence, and if it did we would
# publish a payload leaf that attests a binary that never shipped — a FALSE
# verifiability claim, strictly worse than the unreproducible leaf WS3 replaced.
#
# So the capture build records the compiled binary's fingerprint (the Mach-O / PE
# in target/, plus the macOS externalBin capture-helper sidecar the .app payload
# embeds) and the signed build re-checks it. Match → the payload leaf provably
# wraps the shipped binary. Mismatch → the release stops.
#
# Signing never touches target/<triple>/release/<bin>: the bundler signs a COPY
# placed inside the .app / .exe, so hashing target/ after each build compares
# compile output to compile output — exactly the thing that must not change.
#
# Usage:
#   verify-presig-binary.sh record <target-triple> <out-file>
#   verify-presig-binary.sh verify <target-triple> <recorded-file>
#
# No network, no CI-only tools beyond sha256sum (already required by the attest
# path); scripts/test-attest-helpers.sh exercises record/verify with fixtures.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# sha_file — the canonical lowercase-hex sha256 helper.
# shellcheck source=scripts/lib/payload-hash.sh
. "$here/lib/payload-hash.sh"
# MAIN_BIN — the cargo bin name (`pollis`), the executable inside every bundle.
# shellcheck source=scripts/lib/attest-helpers.sh
. "$here/lib/attest-helpers.sh"

# fingerprint <triple> — echo `<sha256>  <role>` lines (stdout) for the compiled
# main binary and, on macOS, the externalBin capture-helper sidecar. Paths are
# relative to the repo root (the workflow's working directory). Returns 1 with a
# loud annotation if the compiled main binary is missing.
fingerprint() {
  local triple="$1" main sidecar
  main="target/${triple}/release/${MAIN_BIN}"
  case "$triple" in
    *windows*) main="${main}.exe" ;;
  esac
  if [ ! -f "$main" ]; then
    echo "::error::verify-presig-binary: compiled main binary not found at '${main}'" >&2
    echo "::error::  — expected cargo to have built it for ${triple} before this ran." >&2
    return 1
  fi
  printf '%s  main-binary\n' "$(sha_file "$main")"
  # macOS ships the screen-capture helper as an externalBin sidecar staged into
  # the .app payload (src-tauri/build.rs). Windows has no capture sidecar, so this
  # simply matches nothing there.
  sidecar="src-tauri/binaries/pollis-capture-macos-${triple}"
  if [ -f "$sidecar" ]; then
    printf '%s  capture-helper-sidecar\n' "$(sha_file "$sidecar")"
  fi
}

usage() {
  echo "usage: verify-presig-binary.sh record|verify <target-triple> <file>" >&2
  exit 2
}

mode="${1:-}"; triple="${2:-}"; file="${3:-}"
if [ -z "$mode" ] || [ -z "$triple" ] || [ -z "$file" ]; then
  usage
fi
need sha256sum "fingerprint the compiled binary for the WS3 pre-signature check"

case "$mode" in
  record)
    fingerprint "$triple" > "$file"
    echo "verify-presig-binary: recorded pre-signature fingerprint for ${triple}:"
    sed 's/^/  /' "$file"
    ;;
  verify)
    if [ ! -f "$file" ]; then
      echo "::error::verify-presig-binary: no recorded pre-signature fingerprint at" >&2
      echo "::error::  '${file}' — the capture step must run 'record' before the signed build." >&2
      exit 1
    fi
    now="$(fingerprint "$triple")"
    recorded="$(cat "$file")"
    if [ "$now" != "$recorded" ]; then
      echo "::error::WS3 INVARIANT VIOLATED: the signed build's compiled binary does"
      echo "::error::  NOT match the pre-signature payload for ${triple}."
      echo "::error::  Recorded (pre-signature — what the payload leaf hashes):"
      printf '%s\n' "$recorded" | sed 's/^/::error::    /'
      echo "::error::  Now (after the signed build):"
      printf '%s\n' "$now" | sed 's/^/::error::    /'
      echo "::error::  MEANING: the pre-signature payload leaf no longer corresponds to the"
      echo "::error::  binary this release ships. Publishing it would be a FALSE verifiability"
      echo "::error::  claim — a transparency leaf attesting a binary that never shipped, which"
      echo "::error::  is worse than no leaf. The release is STOPPED: do not publish this"
      echo "::error::  payload leaf. A config or toolchain change likely made the capture build"
      echo "::error::  and the signed build diverge; see docs/reproducible-builds-residuals.md"
      echo "::error::  §1 and scripts/verify-presig-binary.sh."
      exit 1
    fi
    echo "verify-presig-binary: OK — the signed build's compiled binary matches the"
    echo "  pre-signature payload for ${triple}; the payload leaf provably wraps it."
    ;;
  *)
    echo "::error::verify-presig-binary: unknown mode '${mode}' (want record|verify)" >&2
    usage
    ;;
esac
