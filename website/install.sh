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

# ── Linux: prefer .deb / .rpm, fall back to AppImage ────────────────────────

install_linux_deb() {
    local deb_url="$1"
    local tmpdir deb_path
    tmpdir=$(mktemp -d)
    deb_path="$tmpdir/pollis.deb"

    info "Downloading $APP_NAME $VERSION (.deb)..."
    curl -fsSL --progress-bar "$deb_url" -o "$deb_path"

    info "Installing .deb package..."
    if sudo dpkg -i "$deb_path"; then
        true
    else
        info "Resolving dependencies..."
        sudo apt-get install -f -y
    fi

    rm -rf "$tmpdir"
    success "$APP_NAME installed via .deb — run 'pollis' to launch."
}

install_linux_rpm() {
    local rpm_url="$1"
    local tmpdir rpm_path
    tmpdir=$(mktemp -d)
    rpm_path="$tmpdir/pollis.rpm"

    info "Downloading $APP_NAME $VERSION (.rpm)..."
    curl -fsSL --progress-bar "$rpm_url" -o "$rpm_path"

    info "Installing .rpm package..."
    if command -v dnf >/dev/null 2>&1; then
        sudo dnf install -y "$rpm_path"
    else
        sudo yum install -y "$rpm_path"
    fi

    rm -rf "$tmpdir"
    success "$APP_NAME installed via .rpm — run 'pollis' to launch."
}

install_linux_appimage() {
    local appimage_url="$1"
    local appimage_dir appimage_path launcher_path desktop_dir desktop_file
    appimage_dir="$HOME/.local/share/pollis"
    appimage_path="$appimage_dir/pollis.AppImage"
    launcher_path="$HOME/.local/bin/pollis"
    desktop_dir="$HOME/.local/share/applications"
    desktop_file="$desktop_dir/pollis.desktop"

    mkdir -p "$appimage_dir" "$HOME/.local/bin" "$desktop_dir"

    info "Downloading $APP_NAME $VERSION (AppImage)..."
    curl -fsSL --progress-bar "$appimage_url" -o "$appimage_path"
    chmod +x "$appimage_path"

    # Create a launcher wrapper that sets WebKit env vars to prevent
    # EGL/compositing crashes on systems without full GPU support.
    cat > "$launcher_path" <<LAUNCHER
#!/usr/bin/env bash
exec env WEBKIT_DISABLE_DMABUF_RENDERER=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 "$appimage_path" "\$@"
LAUNCHER
    chmod +x "$launcher_path"

    # Write a .desktop entry so the app appears in application launchers
    cat > "$desktop_file" <<EOF
[Desktop Entry]
Name=Pollis
Exec=$launcher_path
Icon=pollis
Type=Application
Categories=Network;InstantMessaging;Chat;
StartupNotify=true
EOF

    success "$APP_NAME installed to $launcher_path"

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

install_linux() {
    local deb_url rpm_url appimage_url
    deb_url=$(json_field "$LATEST" "linux_deb")
    rpm_url=$(json_field "$LATEST" "linux_rpm")
    appimage_url=$(json_field "$LATEST" "linux")

    # Prefer native packages: .deb for Debian/Ubuntu, .rpm for Fedora/RHEL
    if [[ -n "$deb_url" ]] && command -v dpkg >/dev/null 2>&1; then
        install_linux_deb "$deb_url"
    elif [[ -n "$rpm_url" ]] && (command -v dnf >/dev/null 2>&1 || command -v yum >/dev/null 2>&1); then
        install_linux_rpm "$rpm_url"
    elif [[ -n "$appimage_url" ]]; then
        info "No supported package manager detected — falling back to AppImage."
        install_linux_appimage "$appimage_url"
    else
        error "No Linux download URL found in latest.json."
    fi
}

case "$OS" in
    Darwin) install_macos ;;
    Linux)  install_linux ;;
    *)      error "Unsupported OS: $OS. Visit https://pollis.com to download manually." ;;
esac
