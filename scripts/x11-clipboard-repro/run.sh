#!/bin/sh
# X11 clipboard repro for #1794. Run on a Linux box from this directory:
#   sudo apt-get install -y xvfb xclip   # once
#   ./run.sh                             # headless, on a private Xvfb display
#   USE_DISPLAY=1 ./run.sh               # against your real X session instead (owner briefly takes CLIPBOARD)
set -u
cd "$(dirname "$0")"
cargo build --bins || exit 1
if [ -z "${USE_DISPLAY:-}" ]; then
  Xvfb :99 -nolisten tcp >/dev/null 2>&1 &
  XVFB=$!
  trap 'kill $XVFB 2>/dev/null' EXIT
  sleep 1
  export DISPLAY=:99
fi
unset WAYLAND_DISPLAY
echo "== unit tests of src-tauri/src/clipboard.rs on Linux =="
timeout 300 cargo test -q --lib 2>&1 | tail -5
SHOT=$(mktemp --suffix=.jpg); echo jpg > "$SHOT"
for mode in ignore refuse anyutf8 slow; do
  echo "== fake Steam owner, mode=$mode =="
  ./target/debug/fake_steam $mode "$SHOT" 2>/tmp/owner-$mode.log &
  OWNER=$!
  sleep 0.5
  echo "xclip TARGETS: $(timeout 10 xclip -selection clipboard -t TARGETS -o 2>&1 | tr '\n' ' ')"
  timeout 120 ./target/debug/probe
  echo "probe exit $?"
  kill $OWNER 2>/dev/null; wait $OWNER 2>/dev/null
  echo "owner saw:"; sort /tmp/owner-$mode.log | uniq -c
done
echo "== no owner at all =="
timeout 120 ./target/debug/probe
rm -f "$SHOT"
