#!/usr/bin/env bash
#
# payload-hash.sh — the ONE canonical payload-hashing implementation, sourced by
# both the release attest job (scripts/attest-binaries.sh) and the independent
# rebuilder (.github/workflows/rebuild-verify.yml).
#
# Reproducibility only works if the party who LOGS a hash and the party who
# INDEPENDENTLY RECOMPUTES it use byte-identical hashing. Two copies of this
# logic would be a silent drift hazard: a rebuilder computing a hash a hair
# differently than the attest job would report a spurious mismatch and cry wolf,
# or (worse) a real divergence could hide behind an incidental formatting
# difference. So there is exactly one copy, here, and everything sources it.
#
# It exposes two functions:
#
#   sha_file <path>
#       sha256 of a single file's bytes, lowercase hex. Used for artifacts whose
#       SHIPPED bytes ARE the reproducible payload (Linux AppImage/deb/rpm — the
#       Tauri minisign signature is detached, so the file itself is the payload).
#
#   sha_tree <dir>
#       Deterministic sha256 of a directory tree: a name-sorted tar with fixed
#       mtime/owner/group so the hash depends ONLY on payload contents, never on
#       filesystem or extraction-order noise. Used for artifacts whose shipped
#       file WRAPS a payload (the macOS `.app` inside a `.dmg`, the unsigned
#       exe+resources inside an NSIS installer). Requires SOURCE_DATE_EPOCH to be
#       exported — the tag commit's unix seconds — so the archive mtime is a
#       deterministic, independently-recoverable value (git), not `now`.
#
# No side effects on source: sourcing this file only defines the two functions.

# sha256 of a single file, lowercase hex.
sha_file() { sha256sum "$1" | awk '{print $1}'; }

# Deterministic sha256 of a directory tree. SOURCE_DATE_EPOCH (tag commit unix
# seconds) fixes the archive mtime so the hash is a pure function of contents.
#
# GNU tar only: `--sort=name` is a GNU extension (BSD/macOS tar lacks it). The
# pre-signature payload for macOS is captured on a macOS runner, which ships BSD
# tar, so that job installs GNU tar and exports `TAR=gtar`; everywhere else the
# default `tar` is already GNU. Both must be GNU tar for the archive bytes — and
# therefore the hash — to match between the party that LOGS and the party that
# REBUILDS (see the file header). Windows captures run under Git-for-Windows bash,
# whose `tar` is already GNU.
sha_tree() {
  : "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH required for sha_tree (tag commit unix seconds)}"
  "${TAR:-tar}" --sort=name --mtime="@${SOURCE_DATE_EPOCH}" \
      --owner=0 --group=0 --numeric-owner \
      -cf - -C "$1" . | sha256sum | awk '{print $1}'
}
