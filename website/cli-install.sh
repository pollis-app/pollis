#!/usr/bin/env bash
set -euo pipefail

# One-line installer for the `pollis` terminal client (the pollis-tui crate).
#   curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash
#
# The download is verified before it is installed, and an unverifiable one is
# refused (see "Verifying what we just downloaded" below). To install anyway —
# not recommended — set POLLIS_SKIP_VERIFY=1:
#   curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | POLLIS_SKIP_VERIFY=1 bash
#
# Mirrors website/install.sh (the desktop installer) in style and
# robustness, but installs a single self-contained CLI binary to
# ~/.local/bin/pollis. The binary links only glibc — SQLCipher's crypto and
# openssl are statically bundled — so there is no libcrypto/libssl runtime
# check here. Windows users grab pollis-windows.exe directly (see below).

LATEST_URL="https://cdn.pollis.com/releases/cli/latest.json"
CDN_BASE="https://cdn.pollis.com/releases/cli"
APP_NAME="Pollis CLI"
BIN_PATH="$HOME/.local/bin/pollis"

BOLD='\033[1m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
RED='\033[0;31m'
RESET='\033[0m'

info()    { echo -e "${BOLD}==> $*${RESET}"; }
success() { echo -e "${GREEN}✓ $*${RESET}"; }
warn()    { echo -e "${YELLOW}! $*${RESET}" >&2; }
error()   { echo -e "${RED}Error: $*${RESET}" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || error "'curl' is required but not installed."

# Minimal JSON field extractor — no jq dependency
json_field() {
    echo "$1" | grep -o "\"$2\":[[:space:]]*\"[^\"]*\"" | sed 's/.*":[[:space:]]*"\(.*\)"/\1/'
}

# ── Verifying what we just downloaded (#877, #1337) ─────────────────────────
#
# The honest limit first, because it decides what is worth checking. This script
# is fetched over TLS from cdn.pollis.com and so is the binary. Any hash we
# published on that same origin would be forgeable by exactly the party a hash
# check is supposed to catch, so a same-origin checksum here would be decoration.
# Both checks below therefore compare the download against something Pollis's
# CDN cannot write:
#
#   1. cosign (the strong path, used whenever it is installed). Every CLI release
#      is signed keylessly by cli-release.yml's GitHub Actions identity, with the
#      signature and its short-lived certificate published next to the binary and
#      the whole thing recorded in Rekor — a public, append-only log Pollis does
#      not operate. cosign checks the certificate chain, the signature and the
#      log inclusion locally.
#
#   2. GitHub's attestation store (no extra tool needed). The same release job
#      records a SLSA build-provenance attestation for each binary with
#      actions/attest-build-provenance, and GitHub serves it, keyed by the
#      binary's SHA-256, at api.github.com/repos/actuallydan/pollis/attestations.
#      Only a workflow running in this repository can put an attestation there.
#      We look the download's SHA-256 up and require an attestation whose
#      subject is THIS file name with THIS digest and whose builder is THIS
#      repository's cli-release.yml. This trusts GitHub (its TLS and its store)
#      rather than verifying the Sigstore bundle locally, which is why cosign
#      stays the stronger path — but it is a value the CDN cannot forge.
#
# Since #1337 an unverified binary is REFUSED by default (it used to warn and
# install anyway when cosign was absent). The documented opt-out is
# POLLIS_SKIP_VERIFY=1, which installs what it could not verify, with a warning.
# A download that is positively contradicted — a bad signature, or an
# attestation for this digest naming some other build — is refused even then.
# POLLIS_REQUIRE_VERIFY=1, the old opt-in, is now the default and still accepted.

REPO_SLUG="actuallydan/pollis"
ATTESTATIONS_API="https://api.github.com/repos/${REPO_SLUG}/attestations"

# SHA-256 of a file, hex. sha256sum on Linux, shasum on macOS.
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        return 1
    fi
}

# BEGIN attestation_matches
# Base64-decode stdin. GNU and current macOS take -d; older macOS only -D.
b64decode() {
    local in
    in=$(cat)
    printf '%s' "$in" | base64 -d 2>/dev/null || printf '%s' "$in" | base64 -D 2>/dev/null
}

# attestation_matches <attestations-json> <published-name> <sha256-hex>
#
# True when one attestation in the GitHub API response is an in-toto statement
# whose subject list holds <published-name> with exactly <sha256-hex>, and whose
# SLSA build definition names this repository's cli-release.yml as the workflow.
# The DSSE payload is base64 (no JSON escaping inside it), so after decoding the
# statement is matched as compact JSON exactly as attest-build-provenance emits
# it. No jq: the installer has no dependency beyond curl and coreutils.
attestation_matches() {
    local json="$1" name="$2" sha="$3" payload statement
    local subject="\"name\":\"${name}\",\"digest\":{\"sha256\":\"${sha}\"}"
    local builder='"repository":"https://github.com/actuallydan/pollis","path":".github/workflows/cli-release.yml"'
    while IFS= read -r payload; do
        # Drop any JSON escaping of the base64 ('\/'); octal 134 is a backslash.
        statement=$(printf '%s' "$payload" | tr -d '\134' | b64decode) || continue
        if [[ "$statement" == *'"predicateType":"https://slsa.dev/provenance/v1"'* ]] \
            && [[ "$statement" == *"$subject"* ]] \
            && [[ "$statement" == *"$builder"* ]]; then
            return 0
        fi
    done < <(printf '%s' "$json" \
        | grep -o '"payload":[[:space:]]*"[^"]*"' \
        | sed 's/.*:[[:space:]]*"\(.*\)"/\1/')
    return 1
}
# END attestation_matches

# Could not verify (as opposed to: verified and wrong). Refuses unless the user
# opted out with POLLIS_SKIP_VERIFY=1.
cannot_verify() {
    local file="$1" why="$2"
    if [[ "${POLLIS_SKIP_VERIFY:-0}" == "1" ]]; then
        warn "${why}
  POLLIS_SKIP_VERIFY=1 is set — installing the UNVERIFIED download anyway."
        return 0
    fi
    rm -f "$file"
    error "${why}
  The download has been deleted and nothing was installed. To fix:
    - install cosign (https://docs.sigstore.dev/cosign/installation/) and re-run, or
    - re-run later if GitHub's API was unreachable or rate-limited, or
    - set POLLIS_SKIP_VERIFY=1 to install without verification (not recommended)."
}

# Positively contradicted: refuse no matter what.
tampered() {
    local file="$1" why="$2"
    rm -f "$file"
    error "${why}
  The download has been deleted. Do not install it.
  Please report this at https://github.com/${REPO_SLUG}/security/advisories/new"
}

# cosign, when it is installed. Returns 1 when it cannot run the check at all
# (no cosign, or no published signature for this release) so the caller falls
# through to the attestation lookup; a signature that does not verify is fatal.
verify_with_cosign() {
    local file="$1" name="$2" tmp sig pem

    if ! command -v cosign >/dev/null 2>&1; then
        return 1
    fi
    tmp=$(mktemp -d)
    sig="$tmp/sig"; pem="$tmp/pem"
    if ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.sig" -o "$sig" \
       || ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.pem" -o "$pem"; then
        rm -rf "$tmp"
        # Releases predating #877 published no signature at all.
        warn "No published Sigstore signature for ${name} at ${VERSION}; trying GitHub's attestation store."
        return 1
    fi

    # --certificate-identity-regexp pins the signer to this repository's release
    # workflow. Without it cosign accepts ANY valid Sigstore identity, which
    # would verify a binary signed by a stranger.
    if ! cosign verify-blob \
        --certificate "$pem" \
        --signature "$sig" \
        --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
        --certificate-identity-regexp '^https://github\.com/actuallydan/pollis/\.github/workflows/cli-release\.yml@' \
        "$file" >/dev/null 2>&1
    then
        rm -rf "$tmp"
        tampered "$file" "SIGNATURE CHECK FAILED for ${name}.
  The published Sigstore signature does not verify these bytes against the Pollis
  release workflow identity."
    fi
    rm -rf "$tmp"
    success "Verified against Sigstore/Rekor (cosign, keyless — no Pollis key involved)"
    return 0
}

verify_download() {
    local file="$1" name="$2" sha json status

    if verify_with_cosign "$file" "$name"; then
        return 0
    fi

    if ! sha=$(sha256_of "$file") || [[ -z "$sha" ]]; then
        cannot_verify "$file" "Neither sha256sum nor shasum is available — the download was NOT verified."
        return 0
    fi

    json="$(dirname "$file")/attestations.json"
    status=$(curl -sS -o "$json" -w '%{http_code}' \
        -H "Accept: application/vnd.github+json" \
        "${ATTESTATIONS_API}/sha256:${sha}" 2>/dev/null) || status="000"
    case "$status" in
        200)
            if attestation_matches "$(cat "$json")" "$name" "$sha"; then
                success "Verified: SHA-256 ${sha} matches the build-provenance attestation GitHub holds for ${REPO_SLUG}'s cli-release workflow"
                return 0
            fi
            tampered "$file" "ATTESTATION MISMATCH for ${name} (sha256 ${sha}).
  GitHub holds an attestation for this digest, but not one naming ${name} as built by
  ${REPO_SLUG}'s cli-release workflow."
            ;;
        404)
            cannot_verify "$file" "No build-provenance attestation exists for ${name} (sha256 ${sha}).
  Either this is not a binary ${REPO_SLUG}'s release workflow built, or it is a release
  older than its attestations — the download was NOT verified."
            ;;
        *)
            cannot_verify "$file" "Could not reach GitHub's attestation API (HTTP ${status}) to check ${name} — the download was NOT verified."
            ;;
    esac
}

# Remove the binary from a previous cli-install.sh run. Preserves user data
# (databases, accounts.json, keystore) under ~/.local/share/pollis/.
cleanup_cli_install() {
    if [[ -e "$BIN_PATH" ]]; then
        rm -f "$BIN_PATH"
        info "Removed the previous pollis CLI binary (user data preserved)."
    fi
}

uninstall() {
    info "Uninstalling $APP_NAME..."
    cleanup_cli_install
    success "$APP_NAME uninstalled. User data preserved at ~/.local/share/pollis/."
}

if [[ "${1:-}" == "uninstall" ]]; then
    uninstall
    exit 0
fi

info "Fetching latest release info..."
LATEST=$(curl -fsSL "$LATEST_URL") || error "Could not reach $LATEST_URL"
VERSION=$(json_field "$LATEST" "version")
info "Latest version: $VERSION"

OS=$(uname -s)
ARCH=$(uname -m)

resolve_url() {
    case "$OS" in
        Linux)
            if [[ "$ARCH" != "x86_64" ]]; then
                error "Only x86_64 Linux is supported at this time (detected: $ARCH)."
            fi
            json_field "$LATEST" "linux"
            ;;
        Darwin)
            if [[ "$ARCH" != "arm64" ]]; then
                error "Only Apple Silicon (arm64) macOS is supported at this time. Intel Mac builds are not yet available."
            fi
            json_field "$LATEST" "macos"
            ;;
        *)
            error "Unsupported OS: $OS. Windows users: download pollis-windows.exe from https://cdn.pollis.com/releases/cli/${VERSION}/pollis-windows.exe"
            ;;
    esac
}

BIN_URL=$(resolve_url)
[[ -n "$BIN_URL" ]] || error "No download URL found in latest.json for this platform."

cleanup_cli_install

mkdir -p "$HOME/.local/bin"

info "Downloading $APP_NAME $VERSION..."
tmpdir=$(mktemp -d)
tmp_bin="$tmpdir/pollis"
curl -fsSL --progress-bar "$BIN_URL" -o "$tmp_bin"

# Verified in the temp directory, BEFORE it is moved into place or made
# executable — an unverified binary never lands on the user's PATH.
verify_download "$tmp_bin" "$(basename "$BIN_URL")"

# Move into place only after a complete download so a failed/partial fetch
# never leaves a broken binary at ~/.local/bin/pollis.
mv "$tmp_bin" "$BIN_PATH"
chmod +x "$BIN_PATH"
rm -rf "$tmpdir"

success "$APP_NAME installed to $BIN_PATH"

# The binary needs only a reasonably modern glibc; a very old distro
# (glibc < 2.35) may be incompatible — rebuild from source there if so.

if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
    echo ""
    echo "  ~/.local/bin is not in your PATH. Add this to your shell profile:"
    echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
    echo ""
    echo "  Then run: pollis"
else
    success "Run 'pollis' to launch."
fi
