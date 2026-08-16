#!/usr/bin/env bash
#
# attest-helpers.sh — the pure, testable helpers `scripts/attest-binaries.sh`
# uses to locate binaries inside extracted packages and to fail loudly.
#
# These live here rather than inline in the attest script so they can be tested
# without a real release. Every attest failure so far (three in a row, #588 /
# #602 / #604) was in exactly this layer — a guessed member path, a missing
# tool, an opaque pipeline — never in the log or leaf design, and none of it was
# reachable by any test because the script's only exercise was a real release.
# See scripts/test-attest-helpers.sh.
#
# Sourcing this file only defines functions and MAIN_BIN; no side effects.

# The main executable's own name inside every bundle shape — Tauri names it from
# the cargo bin (`pollis`), NOT from productName ("Pollis"): the installed app is
# `Pollis.app/Contents/MacOS/pollis`, `pollis.exe`, `usr/bin/pollis`.
MAIN_BIN="${MAIN_BIN:-pollis}"

# need <tool> <why> — fail with a clear message when a tool is absent.
#
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

# find_installed_bin <extracted-root> — locate the installed main executable
# inside an extracted Linux package tree, echoing its path (empty if absent).
#
# Search rather than name the member path: the packagers disagree on the prefix
# (`dpkg-deb --fsys-tarfile` emits `usr/bin/pollis`, rpm's cpio emits
# `./usr/bin/pollis`), and hardcoding one of them failed the v1.5.3 release.
find_installed_bin() {
  find "$1" -type f -path "*/usr/bin/${MAIN_BIN}" 2>/dev/null | head -1
}

# read_payload_sha256 <manifest-file> — echo the validated pre-signature
# payload sha256 (64 lowercase hex) from a build-job manifest, or return 1.
#
# The payload leaf for macOS/Windows must hash the app payload BEFORE
# code-signing / notarization (#603/#704); those bytes exist only in the build
# job, so it captures the pre-signature payload's `sha_tree` and publishes the
# digest in this tiny sidecar. The attest job (which only ever sees the
# already-signed installer) reads the digest from here rather than re-hashing the
# signed artifact — a leaf hashing signed bytes is unreproducible by construction,
# forever, which is worse than no leaf. Pure and side-effect-free so
# test-attest-helpers.sh can exercise the hex-validation without a real release.
read_payload_sha256() {
  local f="${1:-}" sha
  [ -n "$f" ] && [ -f "$f" ] || return 1
  # Whole-file content, all whitespace stripped (tolerate a trailing newline).
  sha="$(tr -d '[:space:]' < "$f")"
  printf '%s' "$sha" | grep -Eq '^[0-9a-f]{64}$' || return 1
  printf '%s' "$sha"
}

# ── The build recipe: real toolchain values, never "unknown" (#877) ───────────
#
# Every BinaryRecord leaf carries a `toolchain` object the docs call "the full
# reproducibility recipe". For 258 leaves across 26 releases (v1.3.4 → v1.9.7)
# that object read `{"rustc":"unknown","node":"unknown","pnpm":"unknown"}`,
# because attest-binaries.sh defaulted the three to "unknown" and no workflow
# ever set them. The leaf is a PERMANENT signed artifact in an append-only tree,
# so the defect compounded release after release and cannot be repaired
# retroactively — the log is append-only by design.
#
# The fix has two halves and both matter:
#
#   1. the values are captured IN THE BUILD JOB, which is the only place that
#      knows what actually compiled the payload (the attest job runs on a
#      different runner and has no rustc/node/pnpm at all — asking it would have
#      recorded the attest runner's toolchain, which built nothing); and
#   2. a missing manifest is a HARD FAILURE, never a default. The silent default
#      is the entire reason the defect ran for 26 releases without anyone
#      noticing: a wrong recipe and a right one produced identical green runs.
#
# The transport is the same tiny-sidecar pattern the payload digest already uses
# (read_payload_sha256 above): the build job writes a small JSON file, uploads it
# with its artifacts, and the release job attaches it to the GitHub release so
# attest-release.yml's no-rebuild backfill path can fetch it too.

# Regex for a recorded version string. Deliberately strict: a value that is not
# a version is a value nobody can install, which is what "unknown" was.
TOOLCHAIN_VERSION_RE='^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.+-]+)?$'

# tool_version <binary> — the exact version of a build tool, or fail loudly.
#
# `rustc --version` prints "rustc 1.96.0 (hash date)", node prints "v20.19.5",
# pnpm prints "10.14.0". Take the first token that looks like a version rather
# than a per-tool field index, then VALIDATE it — a tool whose output format
# shifts must break the release, not quietly record a garbage recipe.
tool_version() {
  local tool="$1" raw ver
  command -v "$tool" >/dev/null 2>&1 || {
    echo "::error::attest: '${tool}' is not on PATH in the build job, so the build" >&2
    echo "::error::  recipe recorded in every transparency leaf for this release" >&2
    echo "::error::  would be incomplete. Every leaf is permanent and append-only," >&2
    echo "::error::  so this fails the release rather than recording 'unknown' (#877)." >&2
    return 1
  }
  raw="$("$tool" --version 2>&1 | head -1)"
  # Strip a leading `v` (node) and pick the first version-shaped token.
  for ver in $raw; do
    ver="${ver#v}"
    if printf '%s' "$ver" | grep -Eq "$TOOLCHAIN_VERSION_RE"; then
      printf '%s' "$ver"
      return 0
    fi
  done
  echo "::error::attest: could not parse a version out of '${tool} --version' " >&2
  echo "::error::  (got: ${raw}). The recipe in every leaf for this release would" >&2
  echo "::error::  be wrong, so the release fails here instead (#877)." >&2
  return 1
}

# runner_image_id — the most specific identity GitHub exposes for the runner
# image, e.g. `ubuntu22@20250804.1.0`.
#
# NOT digest-pinned, because GitHub publishes no digest for a hosted runner
# image: there is no registry reference and no manifest sha to pin. The labels
# the workflow selects with (`ubuntu-22.04`, `macos-latest`) are FLOATING — the
# image behind `ubuntu-22.04` is rebuilt every couple of weeks — which is what
# made the old hardcoded `runner_image` values close to meaningless.
#
# `$ImageOS`/`$ImageVersion` are set on every GitHub-hosted runner and name one
# immutable, dated build of the image (actions/runner-images tags its releases
# with exactly this version). That is the most specific thing available, and it
# is a real improvement on the floating label: an auditor can look up
# `ubuntu22/20250804.1.0` and see the exact package manifest it shipped.
runner_image_id() {
  if [ -z "${ImageOS:-}" ] || [ -z "${ImageVersion:-}" ]; then
    echo "::error::attest: \$ImageOS/\$ImageVersion are unset — this is not a" >&2
    echo "::error::  GitHub-hosted runner, so the runner image cannot be identified." >&2
    echo "::error::  Recording a floating label instead is what #877 exists to fix." >&2
    return 1
  fi
  printf '%s@%s' "$ImageOS" "$ImageVersion"
}

# write_toolchain_manifest <out.json> [extra_runner_id] — capture the recipe.
#
# `extra_runner_id`, when given, is appended to runner_image as
# `+helper:<id>`. The Linux payload genuinely needs it: the AppImage embeds
# pollis-capture-linux, which is compiled in a SEPARATE job on ubuntu-24.04
# (PipeWire 1.0 headers; the app job stays on 22.04 for a low glibc floor). A
# recipe naming only the app runner would send a rebuilder to the wrong image
# for part of the payload — and rebuild-verify.yml already mirrors the split, so
# recording it keeps the published recipe equal to the one that actually works.
write_toolchain_manifest() {
  local out="$1" extra="${2:-}" rustc node pnpm image
  rustc="$(tool_version rustc)" || return 1
  node="$(tool_version node)" || return 1
  pnpm="$(tool_version pnpm)" || return 1
  image="$(runner_image_id)" || return 1
  if [ -n "$extra" ]; then
    image="${image}+helper:${extra}"
  fi
  # Hand-rolled rather than jq: every value is already validated against a strict
  # regex above, and this file is sourced on three different runners where jq's
  # presence is not guaranteed.
  printf '{"rustc":"%s","node":"%s","pnpm":"%s","runner_image":"%s"}\n' \
    "$rustc" "$node" "$pnpm" "$image" > "$out"
  echo "toolchain recipe: rustc ${rustc}, node ${node}, pnpm ${pnpm}, image ${image} -> ${out}"
}

# read_toolchain_field <manifest> <field> — echo one validated field, or fail.
#
# Pure and side-effect-free so test-attest-helpers.sh can exercise the validation
# without a release. Rejects an absent file, an absent field, and a value that
# fails the field's own shape check — a manifest that does not parse must stop
# the attest rather than degrade to a default.
read_toolchain_field() {
  local f="${1:-}" field="${2:-}" val
  [ -n "$f" ] && [ -f "$f" ] && [ -n "$field" ] || return 1
  val="$(tr -d '\n' < "$f" | grep -o "\"${field}\":\"[^\"]*\"" | head -1 | sed 's/.*":"\(.*\)"/\1/')"
  [ -n "$val" ] || return 1
  case "$field" in
    runner_image)
      # `<os>@<version>` with an optional `+helper:<os>@<version>` suffix.
      printf '%s' "$val" \
        | grep -Eq '^[0-9A-Za-z._-]+@[0-9A-Za-z._-]+(\+helper:[0-9A-Za-z._-]+@[0-9A-Za-z._-]+)?$' \
        || return 1
      ;;
    *)
      printf '%s' "$val" | grep -Eq "$TOOLCHAIN_VERSION_RE" || return 1
      ;;
  esac
  # The literal string the old code defaulted to must never round-trip, even if
  # someone hand-writes a manifest containing it.
  [ "$val" != "unknown" ] || return 1
  printf '%s' "$val"
}
