#!/usr/bin/env bash
set -euo pipefail

LATEST_URL="https://cdn.pollis.com/releases/latest.json"
# The transparency log. A DIFFERENT origin from the CDN below, deliberately —
# see the long note above verify_download.
VERIFY_BASE="https://verify.pollis.com"
CDN_BASE="https://cdn.pollis.com/releases"
APP_NAME="Pollis"

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
# BE PRECISE ABOUT WHAT THIS BUYS, because the honest limit matters more than the
# feature. This script is itself fetched over TLS from cdn.pollis.com, and the
# installers come from cdn.pollis.com. A hash published on that SAME origin would
# add nothing: whoever can serve you a swapped binary can serve you a matching
# hash and a modified copy of this script. Every "verify the download" step here
# is therefore built to consult something that is NOT the CDN.
#
# What is actually checked, in order of strength:
#
#   1. The transparency log at verify.pollis.com — a different origin, a
#      different bucket, published by a different workflow with different
#      credentials, as an append-only Merkle tree. We fetch the precomputed
#      report for this release and require that the sha256 of the bytes on disk
#      appears there, bound to this artifact's name and marked included. So a
#      substituted installer needs BOTH hosts compromised, and because the log is
#      append-only, the substitution leaves a permanent public record.
#
#   2. `cosign verify-blob` against Sigstore, when cosign is installed. That is
#      the strongest link available: Rekor is not operated by Pollis at all, and
#      the signing identity is the GitHub Actions workflow itself. Optional
#      because cosign is not something a first-time installer will have.
#
# What this script does NOT do, and does not claim to:
#
#   * It does not check the ML-DSA-44 signature on the log's tree head. A POSIX
#     shell cannot; it trusts TLS for verify.pollis.com. `pollis-verify` checks
#     that signature against a key pinned in the binary, and is the real audit
#     tool — this is a floor, not a ceiling.
#   * It does not authenticate itself. If you do not already trust the channel
#     you fetched this script over, run the checks by hand from
#     https://pollis.com/artifacts.html instead of piping this to bash.
#
# Failure policy: a hash that is present but WRONG aborts hard — that is tamper
# evidence. A log that is unreachable, or a release not yet published into the
# tree (the tree is rebuilt shortly after each release, so there is a window),
# WARNS and continues, because refusing to install when a third-party service is
# down is its own kind of failure. Set POLLIS_REQUIRE_VERIFY=1 to make every
# unverified outcome fatal.

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        return 1
    fi
}

# unverified <message> — honour POLLIS_REQUIRE_VERIFY, else warn and continue.
unverified() {
    if [[ "${POLLIS_REQUIRE_VERIFY:-0}" == "1" ]]; then
        error "$1 (POLLIS_REQUIRE_VERIFY=1)"
    fi
    warn "$1"
    warn "  Continuing. To verify by hand later, see https://pollis.com/artifacts.html"
}

# cosign_verify <file> <artifact_name> — the Sigstore path, when available.
#
# Every release publishes a detached cosign signature and its short-lived signing
# certificate next to the artifact. The certificate identity is pinned to this
# repository's release workflow, so a signature minted by any other identity is
# rejected rather than merely "present".
cosign_verify() {
    local file="$1" name="$2" tmp sig pem
    command -v cosign >/dev/null 2>&1 || return 2
    tmp=$(mktemp -d)
    sig="$tmp/sig"; pem="$tmp/pem"
    if ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.sig" -o "$sig" \
       || ! curl -fsSL "${CDN_BASE}/${VERSION}/${name}.pem" -o "$pem"; then
        rm -rf "$tmp"
        return 2
    fi
    if cosign verify-blob \
        --certificate "$pem" \
        --signature "$sig" \
        --certificate-oidc-issuer "https://token.actions.githubusercontent.com" \
        --certificate-identity-regexp '^https://github\.com/actuallydan/pollis/\.github/workflows/desktop-release\.yml@refs/tags/' \
        "$file" >/dev/null 2>&1
    then
        rm -rf "$tmp"
        return 0
    fi
    rm -rf "$tmp"
    return 1
}

# verify_download <file> <artifact_name>
verify_download() {
    local file="$1" name="$2" sha report

    sha=$(sha256_of "$file") || {
        unverified "Neither sha256sum nor shasum is installed — cannot hash the download."
        return 0
    }
    info "sha256: $sha"

    # Distinguish "the log has no report for this tag" (curl -f exits 22 on an
    # HTTP 4xx) from "the log is unreachable". Both are non-fatal, but they mean
    # different things and telling a user the log is down when the real situation
    # is a release that has not been published into the tree yet sends them
    # looking for the wrong problem.
    local rc=0
    report=$(curl -fsSL --max-time 30 "${VERIFY_BASE}/verify/release/${VERSION}" 2>/dev/null) || rc=$?
    if [[ $rc -eq 22 ]]; then
        unverified "${VERSION} is not published in the transparency log yet — download NOT verified."
        return 0
    elif [[ $rc -ne 0 ]]; then
        unverified "Could not reach the transparency log (${VERIFY_BASE}, curl exit ${rc}) — download NOT verified."
        return 0
    fi

    case "$report" in
        *'"found":true'*) ;;
        *)
            unverified "${VERSION} is not in the transparency log yet — download NOT verified."
            return 0
            ;;
    esac

    # One artifact record per line: the report is compact JSON, so splitting on
    # `{` puts each record on its own line and all three greps below must
    # therefore match within the SAME record. A name and a hash matching in
    # DIFFERENT records would prove nothing.
    #
    # No `grep -q` on the last stage, deliberately: `-q` exits on the first match
    # and SIGPIPEs the greps upstream, which under `pipefail` makes the pipeline
    # report FAILURE precisely when the text was found — here that would raise a
    # tamper alarm on a perfectly good download.
    if ! echo "$report" | tr '{' '\n' \
        | grep -F "\"artifact_name\":\"${name}\"" \
        | grep -F "\"artifact_sha256\":\"${sha}\"" \
        | grep -F '"included":true' >/dev/null
    then
        rm -f "$file"
        error "TAMPER CHECK FAILED for ${name}.
  The file downloaded from the CDN hashes to ${sha}, and the transparency log at
  ${VERIFY_BASE} does not record that hash for ${VERSION}.
  The download has been deleted. Do not install it. Please report this at
  https://github.com/actuallydan/pollis/security/advisories/new"
    fi

    case "$report" in
        *'"chain_valid":true'*)
            success "Verified against the transparency log (${VERSION}, ${name})"
            ;;
        *)
            warn "The log records this exact file, but reports its own chain as INVALID."
            warn "  Run 'pollis-verify' for the full audit before trusting this build."
            ;;
    esac

    # Sigstore: not Pollis-operated, so it is the one link a Pollis compromise
    # cannot forge. Absent cosign is not a failure — it is simply not available.
    if cosign_verify "$file" "$name"; then
        success "Verified against Sigstore/Rekor (cosign, keyless — no Pollis key involved)"
    else
        case "$?" in
            1) error "cosign rejected ${name}: the published signature does not verify
  against the Pollis release workflow identity. Do not install this file." ;;
            *) info "cosign not installed — skipped the Sigstore check (install cosign for the strongest verification)" ;;
        esac
    fi
}

# Remove artifacts from previous install.sh runs. Preserves user data
# (databases, accounts.json, keystore) under ~/.local/share/pollis/.
cleanup_local_install() {
    local removed=0
    local target
    for target in \
        "$HOME/.local/bin/pollis" \
        "$HOME/.local/share/pollis/pollis.AppImage" \
        "$HOME/.local/share/applications/pollis.desktop"
    do
        if [[ -e "$target" ]]; then
            rm -f "$target"
            removed=1
        fi
    done
    if [[ $removed -eq 1 ]]; then
        info "Removed leftover files from a previous install (user data preserved)."
    fi
}

uninstall_linux() {
    info "Uninstalling Pollis..."
    if command -v dpkg >/dev/null 2>&1 && dpkg -l pollis >/dev/null 2>&1; then
        sudo dpkg -r pollis || true
    fi
    if command -v rpm >/dev/null 2>&1 && rpm -q pollis >/dev/null 2>&1; then
        if command -v dnf >/dev/null 2>&1; then
            sudo dnf remove -y pollis || true
        else
            sudo yum remove -y pollis || true
        fi
    fi
    # The AUR package is not removed here — this script never installed it, and
    # silently pacman -R'ing a package the user chose is not ours to do (#821).
    if command -v pacman >/dev/null 2>&1 && pacman -Qq pollis >/dev/null 2>&1; then
        warn "Pollis is also installed from the AUR. Remove that with:  yay -R pollis"
    fi
    cleanup_local_install
    success "Pollis uninstalled. User data preserved at ~/.local/share/pollis/."
}

uninstall_macos() {
    info "Uninstalling Pollis..."
    if [[ -d "/Applications/$APP_NAME.app" ]]; then
        if ! rm -rf "/Applications/$APP_NAME.app" 2>/dev/null; then
            sudo rm -rf "/Applications/$APP_NAME.app"
        fi
    fi
    success "Pollis uninstalled."
}

if [[ "${1:-}" == "uninstall" ]]; then
    case "$(uname -s)" in
        Darwin) uninstall_macos ;;
        Linux)  uninstall_linux ;;
        *)      error "Unsupported OS: $(uname -s)." ;;
    esac
    exit 0
fi

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

    # Before mounting it: a .dmg is only opened after the bytes are known-good.
    verify_download "$dmg_path" "$(basename "$dmg_url")"

    info "Mounting disk image..."
    mount_line=$(hdiutil attach "$dmg_path" -nobrowse -noautoopen | grep "/Volumes/")
    # `hdiutil attach` outputs tab-separated columns, and the volume path is
    # the last column. The DMG volume title can contain a space (e.g.
    # "Pollis 1.2.0"), which breaks `awk '{print $NF}'` (it returns just the
    # last whitespace-token). Strip everything up to the first `/Volumes/`
    # so the entire path, spaces and all, comes through.
    mount_point=$(echo "$mount_line" | sed -E 's|.*(/Volumes/.*)|\1|')

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

    cleanup_local_install

    info "Downloading $APP_NAME $VERSION (.deb)..."
    curl -fsSL --progress-bar "$deb_url" -o "$deb_path"

    # Before handing the file to dpkg, which runs maintainer scripts as root.
    verify_download "$deb_path" "$(basename "$deb_url")"

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

    cleanup_local_install

    info "Downloading $APP_NAME $VERSION (.rpm)..."
    curl -fsSL --progress-bar "$rpm_url" -o "$rpm_path"

    # Before handing the file to dnf/yum, which runs scriptlets as root.
    verify_download "$rpm_path" "$(basename "$rpm_url")"

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

    # Any packaged install owns /usr/bin/pollis, and this launcher will shadow it
    # for anyone whose PATH puts ~/.local/bin first — which is exactly what this
    # script tells them to do below. Warn rather than refuse: outside pacman
    # (handled in handle_pacman_system) we cannot tell a packaged Pollis from a
    # dev `cargo install` of pollis-tui, which is also named `pollis` (#821).
    if [[ -e "/usr/bin/pollis" ]]; then
        warn "/usr/bin/pollis already exists — this AppImage launcher will shadow it"
        warn "  for any shell whose PATH prefers ~/.local/bin."
    fi

    mkdir -p "$appimage_dir" "$HOME/.local/bin" "$desktop_dir"

    info "Downloading $APP_NAME $VERSION (AppImage)..."
    curl -fsSL --progress-bar "$appimage_url" -o "$appimage_path"

    # Verified BEFORE chmod +x: an unverified file never becomes executable.
    verify_download "$appimage_path" "$(basename "$appimage_url")"
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

# Arch and derivatives: defer to the AUR package (#821).
#
# The branch order below picks .deb only when dpkg exists and .rpm only when
# dnf/yum exists, so Arch ALWAYS fell through to the AppImage — which installs a
# launcher at ~/.local/bin/pollis and writes its own pollis.desktop. Alongside
# the AUR package (which owns /usr/bin/pollis and its own desktop entry) that
# gives the user two "Pollis" entries in their launcher and a `pollis` command
# resolving to a stale AppImage, because this script tells them to put
# ~/.local/bin on their PATH. cleanup_local_install() already handles exactly
# this collision, but was only ever called from the deb/rpm paths — install.sh
# had no pacman awareness at all.
#
# Deferring rather than warning: on a pacman system the packaged install is
# strictly better (it updates with the rest of the system and cannot shadow
# anything), so pointing at it is the useful answer, not a second-best. Exit 0,
# not an error — the user asked for Pollis and is being told how to get it.
handle_pacman_system() {
    command -v pacman >/dev/null 2>&1 || return 0

    if [[ "${POLLIS_FORCE_APPIMAGE:-0}" == "1" ]]; then
        warn "pacman detected but POLLIS_FORCE_APPIMAGE=1 — installing the AppImage anyway."
        warn "  If the AUR package is also installed, 'pollis' on your PATH will resolve"
        warn "  to whichever of ~/.local/bin and /usr/bin comes first, and you will have"
        warn "  two Pollis entries in your application menu."
        return 0
    fi

    if pacman -Qq pollis >/dev/null 2>&1; then
        info "Pollis is already installed from the AUR (pacman owns /usr/bin/pollis)."
        echo "  Update it the same way you update everything else:"
        echo "    yay -Syu pollis      # or: paru -Syu pollis"
        echo ""
        echo "  Installing the AppImage on top would shadow it. Nothing was changed."
    else
        info "Arch-based system detected — install Pollis from the AUR:"
        echo "    yay -S pollis        # or: paru -S pollis"
        echo ""
        echo "  The AUR package is maintained by the same release pipeline, installs to"
        echo "  /usr/bin/pollis, and updates with your system. This script's AppImage"
        echo "  fallback would shadow it and duplicate your application-menu entry (#821)."
        echo "  To install the AppImage regardless: POLLIS_FORCE_APPIMAGE=1"
    fi
    # Remove any AppImage a PREVIOUS run of this script left behind, so a user who
    # follows the advice above is not left with the shadowing this exists to stop.
    cleanup_local_install
    exit 0
}

install_linux() {
    local deb_url rpm_url appimage_url
    deb_url=$(json_field "$LATEST" "linux_deb")
    rpm_url=$(json_field "$LATEST" "linux_rpm")
    appimage_url=$(json_field "$LATEST" "linux")

    handle_pacman_system

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
