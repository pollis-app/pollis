#!/usr/bin/env bash
set -euo pipefail

# One-line installer for the `pollis` terminal client (the pollis-tui crate).
#   curl -fsSL https://cdn.pollis.com/releases/cli-install.sh | bash
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

# ── Verifying what we just downloaded (#877) ────────────────────────────────
#
# The honest limit first, because it decides what is worth checking. This script
# is fetched over TLS from cdn.pollis.com and so is the binary. Any hash we
# published on that same origin would be forgeable by exactly the party a hash
# check is supposed to catch, so a same-origin checksum here would be decoration.
#
# What IS worth checking is Sigstore. Since #877 every CLI release is signed
# keylessly by cli-release.yml's GitHub Actions identity, with the signature and
# its short-lived certificate published next to the binary and the whole thing
# recorded in Rekor — a public, append-only log Pollis does not operate. That is
# the one link a compromise of Pollis's own infrastructure cannot forge.
#
# Unlike the desktop installer there is no transparency-log fallback: the
# binaries tree covers desktop bundles only, so the CLI has no leaf to match
# against. `cosign` is therefore the whole check, and when it is absent we say
# plainly that nothing was verified rather than implying otherwise.
# POLLIS_REQUIRE_VERIFY=1 turns "could not verify" into a hard failure.
verify_download() {
    local file="$1" name="$2" tmp sig pem

    if ! command -v cosign >/dev/null 2>&1; then
        local msg="cosign is not installed — the download was NOT verified.
  Install cosign (https://docs.sigstore.dev/cosign/installation/) and re-run, or
  verify by hand:
    cosign verify-blob --certificate ${CDN_BASE}/${VERSION}/${name}.pem \\
      --signature ${CDN_BASE}/${VERSION}/${name}.sig \\
      --certificate-oidc-issuer https://token.actions.githubusercontent.com \\
      --certificate-identity-regexp '^https://github.com/actuallydan/pollis/' <file>"
        if [[ "${POLLIS_REQUIRE_VERIFY:-0}" == "1" ]]; then
            error "$msg"
        fi
        warn "$msg"
        return 0
    fi

    tmp=$(mktemp -d)
    sig="$tmp/sig"; pem="$tmp/pem"
    if ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.sig" -o "$sig" \
       || ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.pem" -o "$pem"; then
        rm -rf "$tmp"
        # Releases predating #877 published no signature at all.
        if [[ "${POLLIS_REQUIRE_VERIFY:-0}" == "1" ]]; then
            error "No published signature for ${name} at ${VERSION} — cannot verify."
        fi
        warn "No published signature for ${name} at ${VERSION} — download NOT verified."
        return 0
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
        rm -f "$file"
        error "SIGNATURE CHECK FAILED for ${name}.
  The published Sigstore signature does not verify these bytes against the Pollis
  release workflow identity. The download has been deleted. Do not install it.
  Please report this at https://github.com/actuallydan/pollis/security/advisories/new"
    fi
    rm -rf "$tmp"
    success "Verified against Sigstore/Rekor (cosign, keyless — no Pollis key involved)"
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
