#!/usr/bin/env bash
# Generate the code-signing identity for over-the-air JS updates (#1250).
# OWNER ONLY, ONCE. NEVER commit the private key.
#
# Writes:
#   ~/.pollis/ota-signing/private-key.pem    RSA private key   -> Doppler, then delete
#   ~/.pollis/ota-signing/public-key.pem     its public half (not needed by anything)
#   mobile/store/ota-code-signing.pem        self-signed X.509 code-signing
#                                            certificate       -> COMMIT this
#
# The certificate is compiled into every prod build (app.config.js,
# `updates.codeSigningCertificate`); the app then refuses any OTA manifest not
# signed by the private key. The key lives in exactly one place that can use
# it: the `ota-signing` GitHub environment (required reviewer), as the
# environment secret OTA_CODE_SIGNING_KEY, synced from Doppler. It is never on
# the update server (the DS Worker at api.pollis.com/updates/ only passes
# pre-signed bytes through).
#
# Uses `expo-updates codesigning:generate` (the expo-updates the app ships), so
# the certificate carries exactly the extensions the app's verifier demands:
# keyUsage digitalSignature, extKeyUsage codeSigning.
#
# Rotating = a new key AND a new store build carrying the new certificate;
# phones on older builds keep trusting only the old one. See mobile/CLAUDE.md
# "OTA updates" and docs/signing-key-compromise-runbook.md.
set -euo pipefail

MOBILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
KEY_DIR="${HOME}/.pollis/ota-signing"
CERT="${MOBILE}/store/ota-code-signing.pem"
# 20 years. expo-updates checks the certificate's validity window on every
# update; an expired certificate silently stops all OTA delivery to the builds
# that carry it, and only a store build can replace it.
YEARS=20

if [ ! -x "${MOBILE}/node_modules/.bin/expo-updates" ] && [ ! -f "${MOBILE}/node_modules/expo-updates/bin/cli.js" ]; then
  echo "error: expo-updates is not installed — run 'npx -y pnpm@10.25.0 install --ignore-workspace' in mobile/ first" >&2
  exit 1
fi

if [ -e "${KEY_DIR}" ] || [ -e "${CERT}" ]; then
  echo "error: ${KEY_DIR} or ${CERT} already exists — refusing to overwrite." >&2
  echo "A new key means a new certificate, which only reaches phones with a new store build;" >&2
  echo "every build in the stores keeps trusting the old one. Move the old files aside first" >&2
  echo "only if that is really what you want." >&2
  exit 1
fi

umask 077
mkdir -p "${KEY_DIR}"
CERT_TMP="$(mktemp -d)"
trap 'rm -rf "${CERT_TMP}"' EXIT

(cd "${MOBILE}" && node node_modules/expo-updates/bin/cli.js codesigning:generate \
  --key-output-directory "${KEY_DIR}" \
  --certificate-output-directory "${CERT_TMP}" \
  --certificate-validity-duration-years "${YEARS}" \
  --certificate-common-name "Pollis OTA updates")

chmod 600 "${KEY_DIR}"/*.pem
umask 022
cp "${CERT_TMP}/certificate.pem" "${CERT}"

# Prove the pair before anyone stores it: sign and verify with the pipeline's
# own code, against the certificate as committed.
(cd "${MOBILE}" && OTA_KEY_FILE="${KEY_DIR}/private-key.pem" node --input-type=module -e '
  import { readFileSync } from "node:fs";
  import { assertCodeSigningCertificate, assertKeyMatchesCertificate } from "./scripts/ota/lib.ts";
  const cert = readFileSync("store/ota-code-signing.pem", "utf8");
  assertCodeSigningCertificate(cert);
  assertKeyMatchesCertificate(readFileSync(process.env.OTA_KEY_FILE, "utf8"), cert);
  console.log("key and certificate match; certificate is a valid code-signing certificate");
')

FINGERPRINT="$(openssl x509 -in "${CERT}" -noout -fingerprint -sha256 | cut -d= -f2)"

cat <<MSG

Generated:
  private key   ${KEY_DIR}/private-key.pem   (mode 600 — SECRET)
  certificate   ${CERT}
  cert SHA-256  ${FINGERPRINT}

Next, in this order:

  1. Store the PRIVATE KEY in Doppler (project pollis, config prd_prod):
       doppler secrets set OTA_CODE_SIGNING_KEY -p pollis -c prd_prod < "${KEY_DIR}/private-key.pem"
     Doppler must sync it ONLY to the GitHub environment \`ota-signing\`
     (Settings → Environments → ota-signing: required reviewer = you, deployment
     branches/tags = main + mobile-ota-*), as the environment secret
     OTA_CODE_SIGNING_KEY — never as a repository secret.
  2. Keep an offline copy (1Password, like the sideload keystore), then delete
     ${KEY_DIR}/private-key.pem from this machine.
  3. Commit mobile/store/ota-code-signing.pem — the public certificate only — and record the
     cert SHA-256 above in the PR. Every store build after that commit can take
     OTA updates; builds before it never will.

Never commit the private key; mobile/.gitignore ignores *.pem except the certificate.
MSG
