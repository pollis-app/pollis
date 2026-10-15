#!/usr/bin/env bash
#
# Put a clang new enough for webrtc-sys on PATH as plain `clang` / `clang++`.
#
# Why (#1303): from webrtc-sys 0.3.43 (every libwebrtc that livekit 0.9 accepts)
# the Linux build compiles its C++ shims against the HERMETIC libc++ baked into
# libwebrtc.a, and that libc++ refuses any compiler older than the clang it was
# built with — currently clang 21 (webrtc-sys reads the floor from the artifact's
# `__configuration/compiler.h`, so a libwebrtc bump can raise it). GCC is
# rejected outright (it ignores libc++'s `trivial_abi`, silently breaking the
# std::unique_ptr calling convention). Ubuntu 22.04 ships clang 14 and 24.04
# clang 18, so every Linux job that builds the desktop media stack runs this.
#
# webrtc-sys calls `clang++` by name unless CXX is set, so this prepends a
# directory of `clang`/`clang++` symlinks to PATH ($GITHUB_PATH on Actions)
# rather than exporting CXX — no new variable reaches the release environment
# (scripts/check-build-recipe.py polices $GITHUB_ENV exports), and the other
# C/C++ builds keep whatever compiler they already chose.
#
# Ubuntu only (CI runners + e2e/Dockerfile). Elsewhere it only checks: on Arch,
# Fedora or Homebrew the distro clang is already new enough, or install one.
#
# Usage: scripts/install-clang.sh            # installs CLANG_MAJOR (default 21)
#        CLANG_MAJOR=22 scripts/install-clang.sh
set -euo pipefail

CLANG_MAJOR="${CLANG_MAJOR:-21}"
SHIM_DIR="${CLANG_SHIM_DIR:-/usr/local/lib/pollis-clang-${CLANG_MAJOR}/bin}"
# apt.llvm.org's archive signing key (Sylvestre Ledru / LLVM). The downloaded
# key is checked against this fingerprint before apt is allowed to trust it.
LLVM_KEY_FPR="6084F3CF814B57C1CF12EFD515CF4D18AF4F7421"

clang_major() {
  "$1" -dM -E -x c++ /dev/null 2>/dev/null | sed -n 's/^#define __clang_major__ //p'
}

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

have="$(command -v clang++ >/dev/null 2>&1 && clang_major clang++ || true)"
if [ -n "$have" ] && [ "$have" -ge "$CLANG_MAJOR" ]; then
  echo "clang++ on PATH is already clang $have (>= $CLANG_MAJOR); nothing to do."
  exit 0
fi

if ! command -v "clang++-${CLANG_MAJOR}" >/dev/null 2>&1; then
  if [ ! -r /etc/os-release ] || ! grep -q '^ID=ubuntu' /etc/os-release; then
    echo "error: clang++ is ${have:-missing}; webrtc-sys needs clang >= ${CLANG_MAJOR}." >&2
    echo "Install it with your package manager (this script only automates Ubuntu)." >&2
    exit 1
  fi
  # shellcheck disable=SC1091
  codename="$(. /etc/os-release && echo "${VERSION_CODENAME}")"

  $SUDO apt-get update
  $SUDO apt-get install -y --no-install-recommends ca-certificates curl gnupg

  keyring="/usr/share/keyrings/apt.llvm.org.gpg"
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL https://apt.llvm.org/llvm-snapshot.gpg.key -o "$tmp/llvm.asc"
  gpg --batch --no-default-keyring --keyring "$tmp/llvm.gpg" --import "$tmp/llvm.asc" 2>/dev/null
  got="$(gpg --batch --no-default-keyring --keyring "$tmp/llvm.gpg" --with-colons --fingerprint \
    | awk -F: '/^fpr:/ {print $10; exit}')"
  if [ "$got" != "$LLVM_KEY_FPR" ]; then
    echo "error: apt.llvm.org key fingerprint is '$got', expected '$LLVM_KEY_FPR'" >&2
    exit 1
  fi
  $SUDO install -m 0644 "$tmp/llvm.gpg" "$keyring"

  echo "deb [signed-by=${keyring}] https://apt.llvm.org/${codename}/ llvm-toolchain-${codename}-${CLANG_MAJOR} main" \
    | $SUDO tee "/etc/apt/sources.list.d/llvm-${CLANG_MAJOR}.list" >/dev/null
  $SUDO apt-get update
  $SUDO apt-get install -y --no-install-recommends "clang-${CLANG_MAJOR}"
fi

$SUDO mkdir -p "$SHIM_DIR"
$SUDO ln -sf "$(command -v "clang-${CLANG_MAJOR}")" "$SHIM_DIR/clang"
$SUDO ln -sf "$(command -v "clang++-${CLANG_MAJOR}")" "$SHIM_DIR/clang++"

if [ -n "${GITHUB_PATH:-}" ]; then
  echo "$SHIM_DIR" >> "$GITHUB_PATH"
else
  # Outside Actions (the e2e image) the caller decides how PATH is set; the
  # Dockerfile sets ENV PATH, a shell can `export PATH="$SHIM_DIR:$PATH"`.
  echo "clang ${CLANG_MAJOR} shims in $SHIM_DIR — put it first on PATH."
fi
"$SHIM_DIR/clang++" --version | head -1
