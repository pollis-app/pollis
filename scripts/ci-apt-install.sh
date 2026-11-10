#!/usr/bin/env bash
#
# `apt-get install` for CI, so a slow runner mirror cannot hold the job.
#
# Why (#1803): ci-apt-retries.sh handles a STALLED connection (30 s timeout,
# retries), but not a TRICKLING one. On 2026-11-06 the runners' Azure Ubuntu
# mirror kept every connection alive at 10-40 KB/s: the nightly's `tests` job sat
# in "Install system dependencies" for 39 minutes and flows shard 0 for 38, until
# both hit their job timeouts and the nightly was red with no test having run.
# Same pattern, shorter, on 11-09 (10-13 min) and 11-10 (33-34 min on one PR).
# A healthy download of this package set takes well under a minute.
#
# So the download is bounded, and only the download: `--download-only` does not
# touch dpkg, so killing it leaves nothing half-installed, and the .debs (and
# partial files) it already fetched stay in apt's cache for the retry. If the
# bound is hit, the Azure mirror is swapped for archive.ubuntu.com in the apt
# sources and the download is retried there. The swap persists for the rest of
# the job, so later apt calls (install-clang.sh) avoid the slow mirror too.
# The retry is not bounded here; the job's timeout-minutes is the backstop.
#
# Usage: scripts/ci-apt-install.sh <package>...
#   APT_FIRST_TRY_SECS   download bound on the runner's own mirror (default 300)
#   APT_FALLBACK_MIRROR  host to switch to (default archive.ubuntu.com)
set -euo pipefail

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <package>..." >&2
  exit 2
fi

FIRST_TRY_SECS="${APT_FIRST_TRY_SECS:-300}"
FALLBACK_MIRROR="${APT_FALLBACK_MIRROR:-archive.ubuntu.com}"
SLOW_MIRROR="azure.archive.ubuntu.com"

SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  SUDO="sudo"
fi

$SUDO apt-get update

start=$(date +%s)
if ! $SUDO timeout "$FIRST_TRY_SECS" apt-get install -y --download-only "$@"; then
  echo "::warning::apt download did not finish in ${FIRST_TRY_SECS}s on the runner's mirror; retrying on ${FALLBACK_MIRROR}."
  # The runner images name the mirror in the deb822 sources and, on some images,
  # in a `mirror+file:` list. Rewrite it wherever it appears.
  switched=0
  for f in /etc/apt/sources.list /etc/apt/sources.list.d/* /etc/apt/apt-mirrors.txt; do
    if [ -f "$f" ] && grep -qF "$SLOW_MIRROR" "$f"; then
      $SUDO sed -i "s#${SLOW_MIRROR}#${FALLBACK_MIRROR}#g" "$f"
      echo "switched $f to ${FALLBACK_MIRROR}"
      switched=1
    fi
  done
  if [ "$switched" -eq 0 ]; then
    echo "no ${SLOW_MIRROR} entry in the apt sources; retrying on the same mirror."
  fi
  $SUDO apt-get update
  $SUDO apt-get install -y --download-only "$@"
fi
echo "apt download took $(( $(date +%s) - start ))s"

# Everything is in the local cache now; this only unpacks and configures.
$SUDO apt-get install -y "$@"
