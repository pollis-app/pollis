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

echo "Building pollis-capture-linux (release, target ${TARGET})"
cargo build -p pollis-capture-linux --release --target "$TARGET"

SRC="target/${TARGET}/release/pollis-capture-linux"
if [ ! -f "$SRC" ]; then
  echo "error: expected helper binary at ${SRC} but it was not produced" >&2
  exit 1
fi

DEST_DIR="src-tauri/binaries"
DEST="${DEST_DIR}/pollis-capture-linux-${TARGET}"
mkdir -p "$DEST_DIR"
cp "$SRC" "$DEST"
chmod +x "$DEST"

echo "Staged sidecar at ${DEST}"
