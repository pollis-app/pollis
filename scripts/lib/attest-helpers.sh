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
