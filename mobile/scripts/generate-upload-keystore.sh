#!/usr/bin/env bash
# Generate the Play upload keystore for Pollis release signing.
# One-time per developer machine. NEVER commit the keystore or its passwords.
set -euo pipefail

KEYSTORE="${HOME}/.pollis/pollis-upload.jks"
ALIAS="pollis-upload"

if ! command -v keytool >/dev/null 2>&1; then
  echo "error: keytool not found — install a JDK (e.g. 'brew install openjdk@17')" >&2
  exit 1
fi

if [ -e "${KEYSTORE}" ]; then
  echo "error: ${KEYSTORE} already exists — refusing to overwrite." >&2
  echo "If you really want a new keystore, move the old one aside first." >&2
  echo "(Losing the upload key means a Play Console key-reset support request.)" >&2
  exit 1
fi

mkdir -p "${HOME}/.pollis"

# RSA-4096, ~27 years validity. keytool prompts for the store password and
# distinguished-name fields interactively; the key password defaults to the
# store password (press RETURN at the key-password prompt).
keytool -genkeypair -v \
  -keystore "${KEYSTORE}" \
  -alias "${ALIAS}" \
  -keyalg RSA \
  -keysize 4096 \
  -validity 10000

chmod 600 "${KEYSTORE}"

echo
echo "Keystore created at ${KEYSTORE}"
echo
echo "Add these lines to ~/.gradle/gradle.properties (create the file if needed),"
echo "filling in the password you just chose:"
echo
echo "POLLIS_UPLOAD_STORE_FILE=${KEYSTORE}"
echo "POLLIS_UPLOAD_STORE_PASSWORD=<store password>"
echo "POLLIS_UPLOAD_KEY_ALIAS=${ALIAS}"
echo "POLLIS_UPLOAD_KEY_PASSWORD=<key password (same as store password if you pressed RETURN)>"
echo
echo "Then 'cd mobile && pnpm expo prebuild -p android' and"
echo "'./gradlew :app:bundleRelease' will sign with this key."
echo "Back the keystore up somewhere safe — and never commit it."
