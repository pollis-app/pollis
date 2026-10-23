#!/usr/bin/env bash
#
# Make apt fail a stalled download fast and retry it, for the rest of a CI job.
#
# Why: the runners' Azure Ubuntu mirror occasionally stalls one connection. With
# apt's defaults (120 s timeout per connection, no retries) that turned a
# 40-second `apt-get install` into 33 minutes on one MLS flows shard, 31 on the
# Kani job and 11 on a Playwright shard — each holding its PR's whole CI run.
# A short timeout plus retries turns the same stall into a few seconds.
#
# Writes an apt.conf.d drop-in, so it covers every later apt call in the job,
# including the ones inside scripts/install-clang.sh and
# `playwright install --with-deps`.
#
# Usage: scripts/ci-apt-retries.sh
set -euo pipefail

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

printf '%s\n' \
  'Acquire::Retries "5";' \
  'Acquire::http::Timeout "30";' \
  'Acquire::https::Timeout "30";' \
  | $SUDO tee /etc/apt/apt.conf.d/80-pollis-ci-retries
