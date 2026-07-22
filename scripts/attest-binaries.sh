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
#   * every bundle EXCEPT the AppImage additionally gets an `exe` leaf holding the
#     sha256 of the main executable as installed. That leaf is what makes the
#     in-app "Verify this build" check work: a running app can hash the file it
#     is executing, but it can reproduce neither a sha_tree of an extracted
#     directory nor the installer file, so `payload` leaves are unmatchable from
#     inside an install. The AppImage needs no `exe` leaf — its shipped bytes ARE
#     the payload and the running app hashes them directly via $APPIMAGE, which
#     is a strictly stronger check (whole payload, not just the main binary).
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
  # One line per leaf. This script runs under `set -e` in a release-critical job,
  # so an unexpected non-zero exit anywhere kills it; without a progress trail the
  # only evidence is "exit code 1" with no indication of which bundle was being
  # processed. Cheap breadcrumbs beat re-running with `bash -x`.
  echo "attest: ${1}/${3} ${5} payload=${6:0:12}… artifact=${7:0:12}…"
}

# Fail with a clear message when a tool this script shells out to is absent.
# Missing tooling otherwise surfaces as an opaque non-zero exit (a subshell
# pipeline under `set -o pipefail` can swallow the diagnostic entirely), which is
# exactly what made the first v1.5.3 attest failure undiagnosable from its log.
need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "::error::attest: required tool '${1}' is not installed — needed to ${2}."
    echo "::error::  Install it in the workflow calling this script. NOTE: both"
    echo "::error::  desktop-release.yml (attest-and-log) and attest-release.yml"
    echo "::error::  run this script and each installs its own tooling."
    exit 1
  }
}

need jq "build the BinaryRecord leaves"
need tar "hash directory-tree payloads"
need sha256sum "hash payloads and artifacts"

find_one() { find "$ARTIFACTS_DIR" -type f -name "$1" 2>/dev/null | head -1; }

# The main executable's own name inside every bundle shape — Tauri names it from
# the cargo bin (`pollis`), NOT from productName ("Pollis"): the installed app is
# `Pollis.app/Contents/MacOS/pollis`, `pollis.exe`, `usr/bin/pollis`.
MAIN_BIN="pollis"

# emit_exe <platform> <arch> <bundle> <artifact_name> <payload_sha> <runner_image> <path-to-exe>
#
# The `exe` leaf: the main executable as installed, hashed alone. This is the
# ONLY leaf a *running* app can reproduce — `payload` leaves are a sha_tree of an
# extracted directory or the installer file, and an installed process has neither
# preimage. Bound to the enclosing payload leaf via the shared payload_sha256
# (the invariant's derived-layer pairing rule), so an exe leaf can never float
# free of a published, reproducible payload.
#
# Hard failure, never a skip: a release whose exe leaf is missing ships an app
# that cannot verify itself, which is precisely the bug this layer exists to fix.
# Better to break the release job loudly than to publish an unverifiable build.
emit_exe() {
  local platform="$1" arch="$2" bundle="$3" name="$4" pay_sha="$5" runner="$6" exe="$7"
  if [ -z "${exe:-}" ] || [ ! -f "$exe" ]; then
    echo "::error::attest: main executable not found for ${bundle} (looked for ${MAIN_BIN}" \
         "at '${exe:-<no match>}') — the in-app build check cannot work for this platform"
    echo "::error::  without it. If the bundle layout changed, update MAIN_BIN or the"
    echo "::error::  extraction for this bundle in scripts/attest-binaries.sh."
    exit 1
  fi
  emit "$platform" "$arch" "$bundle" "$name" exe "$pay_sha" "$(sha_file "$exe")" "$runner"
}

# Locate the installed main executable inside an extracted Linux package tree.
# Search rather than name the member path: the packagers disagree on the prefix
# (`dpkg-deb --fsys-tarfile` emits `usr/bin/pollis`, rpm's cpio emits
# `./usr/bin/pollis`), and hardcoding one of them failed the v1.5.3 release.
find_installed_bin() {
  find "$1" -type f -path "*/usr/bin/${MAIN_BIN}" 2>/dev/null | head -1
}

# ── macOS: .dmg wraps a reproducible .app payload (payload + signed) ──
dmg="$(find_one '*.dmg' || true)"
if [ -n "${dmg:-}" ]; then
  name="pollis-${RELEASE_TAG}-macos.dmg"
  art_sha="$(sha_file "$dmg")"
  need 7z "extract the .app payload from the .dmg"
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
  # The Mach-O the user actually runs, inside the bundle we just hashed.
  emit_exe darwin aarch64 dmg "$name" "$pay_sha" "macos-latest" "$app/Contents/MacOS/$MAIN_BIN"
fi

# ── Windows: NSIS .exe wraps unsigned exe+resources (payload + signed) ──
exe="$(find_one '*.exe' || true)"
if [ -n "${exe:-}" ]; then
  name="pollis-${RELEASE_TAG}-windows.exe"
  art_sha="$(sha_file "$exe")"
  need 7z "extract the file tree from the NSIS installer"
  ex="$work/nsis"; mkdir -p "$ex"
  # 7z unpacks the NSIS installer's embedded file tree.
  7z x -y -o"$ex" "$exe" >/dev/null
  # Drop installer scaffolding that is not part of the reproducible payload.
  rm -rf "$ex/\$PLUGINSDIR" "$ex/Uninstall.exe" 2>/dev/null || true
  pay_sha="$(sha_tree "$ex")"
  emit windows x86_64 nsis "$name" payload "$pay_sha" "$pay_sha" "windows-latest"
  emit windows x86_64 nsis "$name" signed  "$pay_sha" "$art_sha" "windows-latest"
  # NSIS lays the install tree out flat, so the exe sits at the extraction root.
  emit_exe windows x86_64 nsis "$name" "$pay_sha" "windows-latest" "$ex/${MAIN_BIN}.exe"
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
  # Unpack the package filesystem to reach the installed executable — a running
  # deb install's `current_exe()` IS `/usr/bin/pollis`, so that file's hash is
  # what the app will present.
  need dpkg-deb "unpack the .deb to reach its installed executable"
  ex="$work/deb"; mkdir -p "$ex"
  dpkg-deb --fsys-tarfile "$deb" | tar -xf - -C "$ex"
  emit_exe linux x86_64 deb "$name" "$sha" "ubuntu-22.04" "$(find_installed_bin "$ex")"
fi
rpm="$(find_one '*.rpm' || true)"
if [ -n "${rpm:-}" ]; then
  name="pollis-${RELEASE_TAG}-linux.rpm"
  sha="$(sha_file "$rpm")"
  emit linux x86_64 rpm "$name" payload "$sha" "$sha" "ubuntu-22.04"
  # bsdtar (libarchive) reads the .rpm directly. The obvious `rpm2cpio | cpio`
  # was tried first and failed in CI with a bare exit 1 and no diagnostic: cpio
  # was run `--quiet`, and the two-process pipeline under `set -o pipefail` gives
  # no indication of which half died or why. One tool, no pipeline, no cwd
  # juggling, and it handles whatever payload compression the rpm uses.
  need bsdtar "unpack the .rpm to reach its installed executable"
  ex="$work/rpm"; mkdir -p "$ex"
  bsdtar -xf "$rpm" -C "$ex"
  emit_exe linux x86_64 rpm "$name" "$sha" "ubuntu-22.04" "$(find_installed_bin "$ex")"
fi

count="$(jq 'length' <<<"$records")"
[ "$count" -gt 0 ] || { echo "::error::attest: no artifacts found under ${ARTIFACTS_DIR}"; exit 1; }

jq '.' <<<"$records" > "$OUT"
echo "attest: wrote ${count} BinaryRecord leaf/leaves for ${RELEASE_TAG} -> ${OUT}"
