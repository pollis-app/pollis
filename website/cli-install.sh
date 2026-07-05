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
APP_NAME="Pollis CLI"
BIN_PATH="$HOME/.local/bin/pollis"

BOLD='\033[1m'
GREEN='\033[0;32m'
RED='\033[0;31m'
RESET='\033[0m'

info()    { echo -e "${BOLD}==> $*${RESET}"; }
success() { echo -e "${GREEN}✓ $*${RESET}"; }
error()   { echo -e "${RED}Error: $*${RESET}" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || error "'curl' is required but not installed."

# Minimal JSON field extractor — no jq dependency
json_field() {
    echo "$1" | grep -o "\"$2\":[[:space:]]*\"[^\"]*\"" | sed 's/.*":[[:space:]]*"\(.*\)"/\1/'
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
