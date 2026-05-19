#!/usr/bin/env bash
# Build the Linux screen-capture helper and stage it at the Tauri
# `externalBin` sidecar path with the target-triple suffix Tauri requires.
#
# Tauri's externalBin entry is `binaries/pollis-capture-linux` (declared in
# src-tauri/tauri.linux.conf.json, relative to src-tauri/). At build time
# Tauri looks for `<entry>-<target-triple>` and installs it next to the main
# binary as plain `pollis-capture-linux` — which is exactly what
# `locate_helper_binary` in pollis-core probes for in production.
#
# Must run BEFORE `tauri build` for the Linux job only.
set -euo pipefail

TARGET="${1:-x86_64-unknown-linux-gnu}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEST_DIR="src-tauri/binaries"
DEST="${DEST_DIR}/pollis-capture-linux-${TARGET}"

# If the staged sidecar already exists it was supplied by CI's
# download-artifact step (built in the build-capture-helper job on
# ubuntu-24.04, which has PipeWire 1.0.5). Recompiling here would run the
# pipewire/libspa 0.9 build on the ubuntu-22.04 app runner, whose PipeWire
# 0.3.48 headers make libspa fail to compile. Skip the rebuild and use the
# artifact as-is. Local dev: delete this file to force a fresh build.
if [ -f "$DEST" ]; then
  echo "Sidecar already staged at ${DEST} — skipping rebuild (CI artifact)"
  exit 0
fi

echo "Building pollis-capture-linux (release, target ${TARGET})"
cargo build -p pollis-capture-linux --release --target "$TARGET"

SRC="target/${TARGET}/release/pollis-capture-linux"
if [ ! -f "$SRC" ]; then
  echo "error: expected helper binary at ${SRC} but it was not produced" >&2
  exit 1
fi

mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST"
chmod +x "$DEST"

echo "Staged sidecar at ${DEST}"
