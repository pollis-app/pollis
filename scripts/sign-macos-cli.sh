#!/usr/bin/env bash
#
# sign-macos-cli.sh — Developer ID sign + notarize the macOS `pollis` CLI (#1337).
#
# The CLI used to ship ad-hoc signed only: fine through `curl | bash` (curl sets
# no quarantine attribute), but a copy downloaded with a browser is quarantined
# and Gatekeeper blocks it outright. This gives it the same Developer ID
# identity and notarization the desktop .app gets, from the same secrets
# desktop-release.yml already uses (APPLE_CERTIFICATE & co. — read there by
# tauri-action; here by this script, since the CLI is a bare Mach-O and not a
# Tauri bundle).
#
# Usage (CI only, on a macOS runner):
#   scripts/sign-macos-cli.sh <path-to-pollis-binary>
#
# Env: APPLE_CERTIFICATE (base64 .p12), APPLE_CERTIFICATE_PASSWORD,
#      APPLE_SIGNING_IDENTITY ("Developer ID Application: …"), APPLE_ID,
#      APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID. Run
#      `verify-signed-artifact.sh preflight` first so an empty secret fails
#      loudly instead of producing an unsigned binary.
#
# What it does, in order, failing the job on any step:
#   1. imports the certificate into a throwaway keychain (deleted on exit, so
#      the runner never keeps the key);
#   2. `codesign --options runtime --timestamp` — the hardened runtime and a
#      secure timestamp are both notarization requirements;
#   3. asserts the signature verifies and its authority is Developer ID
#      Application (an ad-hoc signature passes `--verify` alone);
#   4. submits to Apple's notary service (credentials from a notarytool
#      profile stored in that keychain, not passed on each command line) and
#      waits; anything but "Accepted" — including notarytool itself failing —
#      fails the job, with Apple's log printed.
#
# A bare executable cannot carry a stapled ticket (`stapler` handles .app,
# .dmg and .pkg only), so Gatekeeper looks the notarization up online the
# first time a quarantined copy runs. That is the documented behaviour for
# notarized command-line tools; it is why step 4 checks Apple's verdict here
# rather than `spctl`, which rejects every bare CLI as "not an app" regardless.
#
# Signing happens BEFORE the artifact is uploaded, so provenance-cli's SLSA
# attestation and cosign signature cover the Developer ID-signed bytes users
# actually download.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
# shellcheck source=scripts/lib/attest-helpers.sh
. "$here/lib/attest-helpers.sh"

bin="${1:-}"
if [ -z "$bin" ] || [ ! -f "$bin" ]; then
  echo "::error::sign-macos-cli: binary not found at '${bin:-<none>}'" >&2
  exit 1
fi

need security "import the Developer ID certificate into a temporary keychain"
need codesign "sign the CLI binary"
need xcrun "submit the CLI to Apple's notary service"
need ditto "zip the CLI for notarization"
need openssl "generate the temporary keychain's password"

work="$(mktemp -d)"
keychain="$work/pollis-cli-signing.keychain-db"
cert="$work/cert.p12"
zip="$work/pollis-cli.zip"
kc_pw="$(openssl rand -hex 24)"

cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

# 1. Throwaway keychain holding only the Developer ID identity.
printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$cert"
security create-keychain -p "$kc_pw" "$keychain"
security set-keychain-settings -lut 3600 "$keychain"
security unlock-keychain -p "$kc_pw" "$keychain"
security import "$cert" -k "$keychain" -P "$APPLE_CERTIFICATE_PASSWORD" -T /usr/bin/codesign >/dev/null
# Let codesign use the key without a GUI prompt (there is no one to click it).
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kc_pw" "$keychain" >/dev/null
# Put it on the search list, keeping whatever was there.
existing="$(security list-keychains -d user | tr -d '"' | xargs)"
# shellcheck disable=SC2086
security list-keychains -d user -s "$keychain" $existing

# 2. Sign: hardened runtime + secure timestamp (notarization requires both).
codesign --force --timestamp --options runtime \
  --identifier com.pollis.cli \
  --keychain "$keychain" \
  --sign "$APPLE_SIGNING_IDENTITY" \
  "$bin"

# 3. The signature verifies and is Developer ID, not ad-hoc.
if ! codesign --verify --strict --verbose=2 "$bin"; then
  echo "::error::sign-macos-cli: codesign --verify --strict FAILED for ${bin}."
  exit 1
fi
details="$(codesign -dvvv "$bin" 2>&1 || true)"
if ! grep -q '^Authority=Developer ID Application' <<<"$details"; then
  echo "::error::sign-macos-cli: ${bin} is not signed by a Developer ID Application authority."
  grep -i '^Authority=' <<<"$details" | sed 's/^/::error::    /' || echo "::error::    (no authority reported)"
  exit 1
fi
if ! grep -q 'flags=.*runtime' <<<"$details"; then
  echo "::error::sign-macos-cli: ${bin} is signed without the hardened runtime; notarization will refuse it."
  exit 1
fi

# 4. Notarize and wait for Apple's verdict.
#
# The app-specific password is handed to notarytool ONCE, to store a profile
# in the throwaway keychain; every later call names the profile, so the
# password is not on the command line of the long-running submit/log calls.
# (`security import` above has no way to take the .p12 password other than
# -P; that keychain and the decoded .p12 are deleted on exit.)
profile="pollis-cli-notary"
xcrun notarytool store-credentials "$profile" \
  --apple-id "$APPLE_ID" \
  --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_PASSWORD" \
  --keychain "$keychain" >/dev/null
ditto -c -k --keepParent "$bin" "$zip"
# Captured rather than allowed to abort under `set -e`: a failed or invalid
# submission is exactly when Apple's log is needed.
submit_rc=0
result="$(xcrun notarytool submit "$zip" \
  --keychain-profile "$profile" \
  --keychain "$keychain" \
  --wait \
  --output-format json)" || submit_rc=$?
status="$(printf '%s' "$result" | grep -o '"status"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/' || true)"
submission="$(printf '%s' "$result" | grep -o '"id"[[:space:]]*:[[:space:]]*"[^"]*"' | head -1 | sed 's/.*"\([^"]*\)"$/\1/' || true)"
if [ "$submit_rc" -ne 0 ] || [ "$status" != "Accepted" ]; then
  echo "::error::sign-macos-cli: notarization failed — status '${status:-unknown}', notarytool exit ${submit_rc} (submission ${submission:-none})."
  printf '%s\n' "$result"
  if [ -n "$submission" ]; then
    xcrun notarytool log "$submission" \
      --keychain-profile "$profile" \
      --keychain "$keychain" || true
  fi
  exit 1
fi

echo "sign-macos-cli: OK — ${bin} is Developer ID signed (hardened runtime) and notarized (submission ${submission})."
