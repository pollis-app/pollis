#!/usr/bin/env bash
# Build the per-platform screen-capture helper and stage it at the Tauri
# `externalBin` sidecar path with the target-triple suffix Tauri requires.
#
# Linux  -> pollis-capture-linux  (externalBin in tauri.linux.conf.json)
# macOS  -> pollis-capture-macos  (externalBin in tauri.macos.conf.json)
#
# Tauri's externalBin entry is `binaries/<helper>` (relative to
# src-tauri/). At build time Tauri looks for `<entry>-<target-triple>`
# and installs it next to the main binary as plain `<helper>` — which is
# exactly what `locate_capture_helper` in pollis-core probes for in
# production.
#
# Must run BEFORE `tauri build`.
set -euo pipefail

# Decide which helper this host builds. An explicit $2 override wins
# (CI cross-builds); otherwise infer from the OS.
OS="$(uname -s)"
case "${OS}" in
  Linux)
    HELPER="pollis-capture-linux"
    DEFAULT_TARGET="x86_64-unknown-linux-gnu"
    ;;
  Darwin)
    HELPER="pollis-capture-macos"
    # Match the app's release target. CI's tauri-action builds with
    # `--target aarch64-apple-darwin`, so the sidecar must carry the
    # same triple suffix for Tauri to find it. `universal-apple-darwin`
    # is also supported (lipo path below) for a local `pnpm build:macos`.
    DEFAULT_TARGET="aarch64-apple-darwin"
    ;;
  *)
    echo "build-capture-helper: unsupported host ${OS} — skipping (Windows captures in-process)"
    exit 0
    ;;
esac

TARGET="${1:-$DEFAULT_TARGET}"
HELPER="${2:-$HELPER}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEST_DIR="src-tauri/binaries"
DEST="${DEST_DIR}/${HELPER}-${TARGET}"

# If the staged sidecar already exists it was supplied by CI's
# download-artifact step (Linux: built on ubuntu-24.04 for PipeWire
# 1.0.5; recompiling on the older app runner would fail libspa). Skip
# the rebuild and use the artifact as-is. Local dev: delete this file
# to force a fresh build.
if [ -f "$DEST" ]; then
  echo "Sidecar already staged at ${DEST} — skipping rebuild (CI artifact)"
  exit 0
fi

echo "Building ${HELPER} (release, target ${TARGET})"

if [ "$TARGET" = "universal-apple-darwin" ]; then
  # Tauri has no `universal-apple-darwin` rustc target; lipo the two
  # arch slices the way `pnpm build:macos` does for the app binary.
  cargo build -p "$HELPER" --release --target aarch64-apple-darwin
  cargo build -p "$HELPER" --release --target x86_64-apple-darwin
  mkdir -p "$DEST_DIR"
  lipo -create \
    "target/aarch64-apple-darwin/release/${HELPER}" \
    "target/x86_64-apple-darwin/release/${HELPER}" \
    -output "$DEST"
  chmod +x "$DEST"
  echo "Staged universal sidecar at ${DEST}"
  exit 0
fi

cargo build -p "$HELPER" --release --target "$TARGET"

SRC="target/${TARGET}/release/${HELPER}"
if [ ! -f "$SRC" ]; then
  echo "error: expected helper binary at ${SRC} but it was not produced" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST"
chmod +x "$DEST"

echo "Staged sidecar at ${DEST}"
