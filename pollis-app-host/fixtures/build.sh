#!/usr/bin/env bash
# Rebuild the fixture components (#2004) reproducibly:
#   fixtures/test-app/  ->  fixtures/test-app.wasm   (the App every test runs)
#   fixtures/wasi-app/  ->  fixtures/wasi-app.wasm   (imports WASI; must be refused)
#
#   ./build.sh          rebuild and overwrite the checked-in components
#   ./build.sh --check  rebuild and fail if either differs from the checked-in one
#
# Pinned: the repo's Rust (rust-toolchain.toml), the guest's Cargo.lock
# (--locked), and the source path remapped out of the binary, so the same
# commit gives the same bytes on any machine.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
toolchain="$(sed -n 's/^channel = "\(.*\)"/\1/p' "$repo/rust-toolchain.toml")"

if ! rustup target list --installed --toolchain "$toolchain" | grep -qx wasm32-wasip2; then
  rustup target add wasm32-wasip2 --toolchain "$toolchain"
fi

target_dir="$(mktemp -d)"
trap 'rm -rf "$target_dir"' EXIT
status=0

for app in test-app wasi-app; do
  (
    cd "$here/$app"
    RUSTFLAGS="--remap-path-prefix=$repo=/pollis --remap-path-prefix=$HOME/.cargo=/cargo -C debuginfo=0" \
    SOURCE_DATE_EPOCH=0 \
      cargo "+$toolchain" build --quiet --locked --release --target wasm32-wasip2 --target-dir "$target_dir/$app"
  )
  built="$target_dir/$app/wasm32-wasip2/release/pollis_${app//-/_}.wasm"
  out="$here/$app.wasm"
  if [[ "${1:-}" == "--check" ]]; then
    if cmp -s "$built" "$out"; then
      echo "$app.wasm is up to date ($(sha256sum "$out" | cut -c1-16))"
    else
      echo "$app.wasm differs from a fresh build of fixtures/$app; run $0" >&2
      status=1
    fi
  else
    cp "$built" "$out"
    echo "wrote $out ($(wc -c <"$out") bytes, sha256 $(sha256sum "$out" | cut -c1-16))"
  fi
done
exit $status
