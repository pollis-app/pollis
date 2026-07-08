#!/usr/bin/env bash
#
# attest-binaries.sh — emit binary-transparency BinaryRecord leaves for one
# released tag (issue #453, Phase 2 "attest-and-log").
#
# For each shipped platform artifact it computes:
#   * payload_sha256  — the reproducible PRE-SIGNATURE payload (the `.app`
#     contents inside a `.dmg`, the unsigned exe+resources inside an NSIS
#     installer, the AppImage/deb/rpm payload) hashed deterministically;
#   * artifact_sha256 — the SHIPPED, signed file's own sha256.
# and emits the leaves the `binaries` tenant consumes (see
# verifiable-log-builder/src/binaries.rs, matched field-for-field):
#   * platforms whose shipped file embeds a signature (macOS dmg, Windows nsis)
#     get TWO leaves — a `payload` leaf then a `signed` leaf, joined by the
#     shared payload_sha256 (payload FIRST, so the invariant's payload/signed
#     pairing rule is satisfied);
#   * platforms whose shipped bytes ARE the reproducible payload (Linux
#     AppImage/deb/rpm — Tauri's minisign signature is detached, and deb/rpm are
#     not signed here) get ONE `payload` leaf with artifact_sha256 == payload_sha256.
#
# Output: a JSON array of BinaryRecords (this tag only) at $OUT, in publish
# order. The caller (desktop-release.yml) merges it into the accumulating
# records JSON on R2 and hands that to `builder build-binaries`.
#
# Reproducibility caveat (P2 vs P5): the payload EXTRACTION here is best-effort
# on a Linux runner (7z over dmg/NSIS) and the toolchain recipe is recorded from
# labels, not digest-pinned. Byte-exact reproducibility + digest-pinned runners
# are Phase 5 (docs/verifiable-builds-design.md §1.5, §6). What P2 delivers is
# the correct LEAF STRUCTURE and the two hashes per artifact.
set -euo pipefail

: "${RELEASE_TAG:?RELEASE_TAG required (e.g. v1.3.0)}"
: "${COMMIT:?COMMIT required (40-hex git sha)}"
: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH required (tag commit unix seconds)}"
: "${ARTIFACTS_DIR:?ARTIFACTS_DIR required (downloaded artifact root)}"
: "${OUT:?OUT required (output records JSON path)}"

RUSTC_VERSION="${RUSTC_VERSION:-unknown}"
NODE_VERSION="${NODE_VERSION:-unknown}"
PNPM_VERSION="${PNPM_VERSION:-unknown}"
PROVENANCE_BASE="${PROVENANCE_BASE:-cdn.pollis.com/releases/${RELEASE_TAG}}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

records="[]"

# sha_file / sha_tree — the ONE canonical payload-hashing implementation, shared
# byte-for-byte with the independent rebuilder (.github/workflows/rebuild-verify.yml)
# via this sourced helper. The party that LOGS a hash and the party that
# INDEPENDENTLY RECOMPUTES it MUST hash identically, or a rebuilder cries wolf on
# an incidental formatting difference. One copy, sourced here and there.
# (sha_tree reads SOURCE_DATE_EPOCH — already required above.)
# shellcheck source=scripts/lib/payload-hash.sh
. "$(dirname "$0")/lib/payload-hash.sh"

# emit <platform> <arch> <bundle> <artifact_name> <layer> <payload_sha> <artifact_sha> <runner_image>
emit() {
  local rec
  rec="$(jq -n \
    --arg release_tag "$RELEASE_TAG" \
    --arg commit "$COMMIT" \
    --arg platform "$1" --arg arch "$2" --arg bundle "$3" \
    --arg artifact_name "$4" --arg layer "$5" \
    --arg payload_sha256 "$6" --arg artifact_sha256 "$7" \
    --arg rustc "$RUSTC_VERSION" --arg node "$NODE_VERSION" --arg pnpm "$PNPM_VERSION" \
    --arg runner_image "$8" \
    --argjson source_date_epoch "$SOURCE_DATE_EPOCH" \
    --arg provenance_uri "${PROVENANCE_BASE}/${4}.intoto.jsonl" \
    '{release_tag:$release_tag, commit:$commit, platform:$platform, arch:$arch,
      bundle:$bundle, artifact_name:$artifact_name, layer:$layer,
      payload_sha256:$payload_sha256, artifact_sha256:$artifact_sha256,
      toolchain:{rustc:$rustc, node:$node, pnpm:$pnpm, runner_image:$runner_image,
                 source_date_epoch:$source_date_epoch},
      provenance_uri:$provenance_uri}')"
  records="$(jq -c ". + [${rec}]" <<<"$records")"
}

find_one() { find "$ARTIFACTS_DIR" -type f -name "$1" 2>/dev/null | head -1; }

# ── macOS: .dmg wraps a reproducible .app payload (payload + signed) ──
dmg="$(find_one '*.dmg' || true)"
if [ -n "${dmg:-}" ]; then
  name="pollis-${RELEASE_TAG}-macos.dmg"
  art_sha="$(sha_file "$dmg")"
  ex="$work/dmg"; mkdir -p "$ex"
  # 7z reads the HFS filesystem inside the .dmg on Linux; extract the .app.
  # The .dmg carries the standard "drag to /Applications" symlink, on which
  # p7zip prints "Dangerous link path was ignored" and returns a non-zero exit
  # even though it correctly skips only that symlink and extracts the .app. We
  # don't need that symlink, so tolerate the exit and let the .app-present check
  # below be the real gate (a genuine extraction failure leaves no .app).
  7z x -y -o"$ex" "$dmg" >/dev/null 2>&1 || true
  app="$(find "$ex" -maxdepth 4 -name '*.app' -type d | head -1 || true)"
  [ -n "${app:-}" ] || { echo "::error::attest: no .app payload found inside ${dmg}"; exit 1; }
  pay_sha="$(sha_tree "$app")"
  emit darwin aarch64 dmg "$name" payload "$pay_sha" "$pay_sha" "macos-latest"
  emit darwin aarch64 dmg "$name" signed  "$pay_sha" "$art_sha" "macos-latest"
fi

# ── Windows: NSIS .exe wraps unsigned exe+resources (payload + signed) ──
exe="$(find_one '*.exe' || true)"
if [ -n "${exe:-}" ]; then
  name="pollis-${RELEASE_TAG}-windows.exe"
  art_sha="$(sha_file "$exe")"
  ex="$work/nsis"; mkdir -p "$ex"
  # 7z unpacks the NSIS installer's embedded file tree.
  7z x -y -o"$ex" "$exe" >/dev/null
  # Drop installer scaffolding that is not part of the reproducible payload.
  rm -rf "$ex/\$PLUGINSDIR" "$ex/Uninstall.exe" 2>/dev/null || true
  pay_sha="$(sha_tree "$ex")"
  emit windows x86_64 nsis "$name" payload "$pay_sha" "$pay_sha" "windows-latest"
  emit windows x86_64 nsis "$name" signed  "$pay_sha" "$art_sha" "windows-latest"
fi

# ── Linux: the shipped bytes ARE the reproducible payload (payload-only) ──
appimage="$(find_one '*.AppImage' || true)"
if [ -n "${appimage:-}" ]; then
  name="pollis-${RELEASE_TAG}-linux.AppImage"
  sha="$(sha_file "$appimage")"
  emit linux x86_64 appimage "$name" payload "$sha" "$sha" "ubuntu-22.04"
fi
deb="$(find_one '*.deb' || true)"
if [ -n "${deb:-}" ]; then
  name="pollis-${RELEASE_TAG}-linux.deb"
  sha="$(sha_file "$deb")"
  emit linux x86_64 deb "$name" payload "$sha" "$sha" "ubuntu-22.04"
fi
rpm="$(find_one '*.rpm' || true)"
if [ -n "${rpm:-}" ]; then
  name="pollis-${RELEASE_TAG}-linux.rpm"
  sha="$(sha_file "$rpm")"
  emit linux x86_64 rpm "$name" payload "$sha" "$sha" "ubuntu-22.04"
fi

count="$(jq 'length' <<<"$records")"
[ "$count" -gt 0 ] || { echo "::error::attest: no artifacts found under ${ARTIFACTS_DIR}"; exit 1; }

jq '.' <<<"$records" > "$OUT"
echo "attest: wrote ${count} BinaryRecord leaf/leaves for ${RELEASE_TAG} -> ${OUT}"
