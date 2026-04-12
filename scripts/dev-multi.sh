#!/usr/bin/env bash
# Spin up 4 Tauri dev instances for multi-device testing.
#
#   dan-d1  dankral01@gmail.com       (default data dir)
#   dan-d2  dankral01@gmail.com       (separate data dir = second device)
#   ants    dreamsofants@gmail.com
#   guy     person.guy@mail.com
#
# Usage:  ./scripts/dev-multi.sh
# Stop:   Ctrl-C (kills all four)

set -euo pipefail
cd "$(dirname "$0")/.."

exec concurrently --kill-others \
  -n "dan-d1,dan-d2,ants,guy" \
  -c "green,blue,yellow,magenta" \
  "DEV_EMAIL=dankral01@gmail.com pnpm dev" \
  "DEV_EMAIL=dankral01@gmail.com POLLIS_DATA_DIR=/tmp/pollis-dan-d2 pnpm dev" \
  "DEV_EMAIL=dreamsofants@gmail.com POLLIS_DATA_DIR=/tmp/pollis-ants pnpm dev" \
  "DEV_EMAIL=person.guy@mail.com POLLIS_DATA_DIR=/tmp/pollis-guy pnpm dev"
