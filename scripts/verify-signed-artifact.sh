#!/usr/bin/env bash
#
# verify-signed-artifact.sh — WS3 (#603/#704): make "the artifact we ship is
# signed" something CI CHECKS, not an assumption sitting three lines below the
# payload-leaf check that verify-presig-binary.sh already enforces.
#
# WS3 removed the redundant hardcoded macOS signingIdentity from the committed
# Tauri config (#704 — it was overridden by APPLE_SIGNING_IDENTITY anyway), which
# is correct, but it means the ENTIRE macOS signing configuration now lives in one
# secret. If APPLE_SIGNING_IDENTITY is ever empty or unset, tauri resolves no
# identity, SKIPS codesigning AND notarization, and the job still goes green —
# shipping a .dmg Gatekeeper blocks on every user's machine, wrapped in a
# valid-looking payload leaf and attestation. Nothing downstream of the signed
# build asserted the artifact is actually signed. This script is that assertion.
#
# Two independent guards, both mandatory in desktop-release.yml:
#   preflight  — BEFORE the signed build: hard-fail if a required signing secret
#                is empty. GitHub Actions DEFINES the env var for every listed
#                secret and sets it to the EMPTY STRING when the secret is unset,
#                so a missing secret reaches the runner as `Some("")`, not `None`;
#                this check makes that distinction irrelevant forever — empty is a
#                failure, full stop, and we never depend on how the bundler's own
#                env filter treats an empty identity.
#   macos/windows — AFTER the signed build: prove the SHIPPED artifact carries a
#                signature the platform gatekeeper will accept, the same way
#                verify-presig-binary.sh proves the payload leaf wraps the binary.
#                  * macOS: codesign --verify --deep --strict, a Developer ID
#                    Application authority (rejects an ad-hoc / empty-identity
#                    signature that --verify alone tolerates), and spctl Gatekeeper
#                    acceptance (which additionally requires a valid notarization
#                    staple). The authority is asserted by PREFIX only — no team id
#                    or identity string is written here.
#                  * Windows: signtool verify /pa — the Authenticode signature
#                    chains to a trusted root under the default policy a normal
#                    machine uses, with a valid RFC-3161 timestamp.
#
# The platform tools (codesign/spctl/signtool) exist only on their own runners, so
# the `macos`/`windows` modes are exercised by a real release, not on this Linux
# box; scripts/test-attest-helpers.sh covers the `preflight` logic and the
# dispatch/guards that run anywhere. A new file rather than a mode of
# verify-presig-binary.sh: that script's tight record/verify(<triple> <file>)
# contract is a different operation (does the payload leaf wrap the binary) from
# this one (is the shipped artifact signed), with a different arg shape.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# need — the shared "fail loudly, naming the tool" helper.
# shellcheck source=scripts/lib/attest-helpers.sh
. "$here/lib/attest-helpers.sh"

usage() {
  echo "usage: verify-signed-artifact.sh preflight <label> <ENV_VAR> [<ENV_VAR> ...]" >&2
  echo "       verify-signed-artifact.sh macos   <app-path>" >&2
  echo "       verify-signed-artifact.sh windows <exe-path> [<signtool-path>]" >&2
  exit 2
}

mode="${1:-}"

case "$mode" in
  preflight)
    label="${2:-}"
    if [ -z "$label" ] || [ "$#" -lt 3 ]; then
      echo "::error::verify-signed-artifact: preflight needs <label> and >=1 env var name" >&2
      usage
    fi
    shift 2
    missing=0
    for var in "$@"; do
      # Indirect expansion with a default so an UNSET var reads as empty rather
      # than tripping `set -u` — an unset secret and an empty secret are the same
      # failure here, which is the whole point.
      if [ -z "${!var-}" ]; then
        echo "::error::WS3: ${label} signing secret ${var} is empty or unset."
        echo "::error::  Refusing to run the signed build: with ${var} empty the bundler"
        echo "::error::  resolves no identity, SKIPS signing AND notarization, and still"
        echo "::error::  exits 0 — publishing an unsigned/unnotarized artifact the platform"
        echo "::error::  gatekeeper rejects, wrapped in a valid-looking payload leaf and"
        echo "::error::  attestation. An unsigned artifact must not be published (#704)."
        missing=1
      fi
    done
    [ "$missing" -eq 0 ] || exit 1
    echo "verify-signed-artifact: preflight OK — ${label} signing secrets are all present."
    ;;

  macos)
    app="${2:-}"
    if [ -z "$app" ] || [ ! -d "$app" ]; then
      echo "::error::verify-signed-artifact: macOS .app bundle not found at '${app:-<none>}'" >&2
      exit 1
    fi
    need codesign "verify the shipped macOS code signature"
    need spctl "verify Gatekeeper accepts the notarized macOS artifact"
    # 1. Structural signature validity over the whole bundle.
    if ! codesign --verify --deep --strict --verbose=2 "$app"; then
      echo "::error::WS3: codesign --verify --deep --strict FAILED for ${app} — the shipped"
      echo "::error::  .app is not validly signed. An unsigned/broken-signature artifact must"
      echo "::error::  not be published (#704)."
      exit 1
    fi
    # 2. Signed by a Developer ID Application authority. --verify above accepts an
    #    ad-hoc or empty-identity signature; this rejects exactly the silent
    #    empty-APPLE_SIGNING_IDENTITY outcome. Assert the authority PREFIX only —
    #    no team id / identity string is hardcoded (the Developer ID string was
    #    removed from the config in #704 and must not reappear, here or anywhere).
    authority="$(codesign -dvvv "$app" 2>&1 || true)"
    if ! grep -q '^Authority=Developer ID Application' <<<"$authority"; then
      echo "::error::WS3: ${app} is NOT signed by a Developer ID Application authority —"
      echo "::error::  Gatekeeper would reject it (an ad-hoc or empty-identity signature"
      echo "::error::  passes codesign --verify but is not shippable). Authority chain seen:"
      grep -i '^Authority=' <<<"$authority" | sed 's/^/::error::    /' || echo "::error::    (none reported)"
      exit 1
    fi
    # 3. Gatekeeper acceptance — the check that most directly models "the user's
    #    Mac will run it": it additionally requires a valid, stapled notarization
    #    ticket, so an un-notarized Developer ID build fails here.
    if ! spctl --assess --type exec --verbose=4 "$app"; then
      echo "::error::WS3: spctl rejected ${app} — Gatekeeper will not accept it (missing or"
      echo "::error::  invalid notarization staple). An un-notarized artifact must not be"
      echo "::error::  published (#704)."
      exit 1
    fi
    echo "verify-signed-artifact: OK — ${app} is Developer ID signed and Gatekeeper-accepted."
    ;;

  windows)
    exe="${2:-}"
    signtool="${3:-${SIGNTOOL_PATH:-}}"
    if [ -z "$exe" ] || [ ! -f "$exe" ]; then
      echo "::error::verify-signed-artifact: Windows installer .exe not found at '${exe:-<none>}'" >&2
      exit 1
    fi
    if [ -z "$signtool" ] || [ ! -f "$signtool" ]; then
      echo "::error::verify-signed-artifact: signtool not found (SIGNTOOL_PATH='${SIGNTOOL_PATH:-}'," >&2
      echo "::error::  arg='${3:-}'). The Windows job resolves SIGNTOOL_PATH earlier in the run;" >&2
      echo "::error::  pass it or export it before this step." >&2
      exit 1
    fi
    # signtool verify /pa: validate under the default Authenticode policy a normal
    # machine uses — the signature chains to a trusted root and the RFC-3161
    # timestamp is valid. /v for a verbose chain in the log.
    if ! "$signtool" verify /pa /v "$exe"; then
      echo "::error::WS3: signtool verify /pa FAILED for ${exe} — the shipped installer is not"
      echo "::error::  validly Authenticode-signed. An unsigned artifact must not be published (#704)."
      exit 1
    fi
    echo "verify-signed-artifact: OK — ${exe} carries a valid Authenticode signature."
    ;;

  *)
    echo "::error::verify-signed-artifact: unknown mode '${mode}' (want preflight|macos|windows)" >&2
    usage
    ;;
esac
