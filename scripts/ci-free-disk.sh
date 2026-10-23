#!/usr/bin/env bash
#
# Free runner disk for the jobs that link the full native stack (Tauri +
# libwebrtc + livekit), but only when the disk is actually short.
#
# Why conditional: the cleanup below (rm -rf of preinstalled toolchains plus a
# docker image prune) took 2-6 minutes per job, and it ran unconditionally on
# five Linux jobs of every Rust PR. The ubuntu-24.04 runners now have a 145 GB
# root with ~90 GB free before any cleanup, which is far more than a cold
# workspace build needs, so on today's runners this is a no-op. It stays as a
# guard: if a runner image ever ships small again, the link-time `ld ... signal
# 7 [Bus error]` (disk exhaustion) this was added for cannot come back.
#
# Usage: scripts/ci-free-disk.sh            # needs MIN_FREE_GB (default 40) free
#        MIN_FREE_GB=60 scripts/ci-free-disk.sh
set -euo pipefail

MIN_FREE_GB="${MIN_FREE_GB:-40}"

free_gb() {
  df -BG --output=avail / | tail -n 1 | tr -dc '0-9'
}

df -h /
have="$(free_gb)"
if [ "$have" -ge "$MIN_FREE_GB" ]; then
  echo "${have} GB free (>= ${MIN_FREE_GB} GB): no cleanup needed."
  exit 0
fi

echo "Only ${have} GB free (< ${MIN_FREE_GB} GB): removing unused preinstalled toolchains."
sudo rm -rf /usr/share/dotnet /opt/ghc /usr/local/lib/android \
  /usr/local/share/boost /opt/hostedtoolcache/CodeQL /usr/local/.ghcup || true
sudo docker image prune --all --force || true
df -h /
