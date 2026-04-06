#!/usr/bin/env bash
set -euo pipefail

LATEST_URL="https://cdn.pollis.com/releases/latest.json"
APP_NAME="Pollis"

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

info "Fetching latest release info..."
LATEST=$(curl -fsSL "$LATEST_URL") || error "Could not reach $LATEST_URL"
VERSION=$(json_field "$LATEST" "version")
info "Latest version: $VERSION"

OS=$(uname -s)

install_macos() {
    local arch
    arch=$(uname -m)
    if [[ "$arch" != "arm64" ]]; then
        error "Only Apple Silicon (arm64) is supported at this time. Intel Mac builds are not yet available."
    fi

    local dmg_url tmpdir dmg_path mount_line mount_point
    dmg_url=$(json_field "$LATEST" "macos")
    tmpdir=$(mktemp -d)
    dmg_path="$tmpdir/Pollis.dmg"

    info "Downloading $APP_NAME $VERSION..."
    curl -fsSL --progress-bar "$dmg_url" -o "$dmg_path"

    info "Mounting disk image..."
    mount_line=$(hdiutil attach "$dmg_path" -nobrowse -noautoopen | grep "/Volumes/")
    mount_point=$(echo "$mount_line" | awk '{print $NF}')

    info "Installing to /Applications..."
    if [[ -d "/Applications/$APP_NAME.app" ]]; then
        rm -rf "/Applications/$APP_NAME.app"
    fi

    if cp -R "$mount_point/$APP_NAME.app" /Applications/ 2>/dev/null; then
        true
    else
        info "Permission denied — retrying with sudo..."
        sudo cp -R "$mount_point/$APP_NAME.app" /Applications/
    fi

    hdiutil detach "$mount_point" -quiet
    rm -rf "$tmpdir"

    success "$APP_NAME installed to /Applications/$APP_NAME.app"
}

install_linux() {
    local appimage_url install_dir install_path desktop_dir desktop_file
    appimage_url=$(json_field "$LATEST" "linux")
    install_dir="$HOME/.local/bin"
    install_path="$install_dir/pollis"
    desktop_dir="$HOME/.local/share/applications"
    desktop_file="$desktop_dir/pollis.desktop"

    mkdir -p "$install_dir" "$desktop_dir"

    info "Downloading $APP_NAME $VERSION..."
    curl -fsSL --progress-bar "$appimage_url" -o "$install_path"
    chmod +x "$install_path"

    # Write a .desktop entry so the app appears in application launchers
    cat > "$desktop_file" <<EOF
[Desktop Entry]
Name=Pollis
Exec=$install_path
Icon=pollis
Type=Application
Categories=Network;InstantMessaging;Chat;
StartupNotify=true
EOF

    success "$APP_NAME installed to $install_path"

    if [[ ":$PATH:" != *":$HOME/.local/bin:"* ]]; then
        echo ""
        echo "  ~/.local/bin is not in your PATH. Add this to your shell profile:"
        echo "    export PATH=\"\$HOME/.local/bin:\$PATH\""
        echo ""
        echo "  Then run: pollis"
    else
        success "Run 'pollis' to launch."
    fi
}

case "$OS" in
    Darwin) install_macos ;;
    Linux)  install_linux ;;
    *)      error "Unsupported OS: $OS. Visit https://pollis.com to download manually." ;;
esac
