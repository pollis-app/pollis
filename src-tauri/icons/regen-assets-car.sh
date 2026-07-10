#!/usr/bin/env bash
# Rebuild icons/Assets.car from the Icon Composer source (logo-lock-2.icon).
#
# The macOS 26 Liquid Glass icon ships as a pre-compiled Assets.car that is
# committed to the repo and referenced from tauri.conf.json's bundle.icon. We
# compile it here — once, locally — instead of at build time, because actool is
# flaky on the CI runners (it intermittently fails to emit Assets.car). The
# Tauri bundler copies an existing .car verbatim, so CI needs no Xcode/actool.
#
# Run this on macOS with Xcode 26+ whenever logo-lock-2.icon changes, then
# commit the regenerated Assets.car. The flags mirror tauri-bundler's own
# actool invocation (crates/tauri-bundler/src/bundle/macos/icon.rs) so the
# committed artifact matches what the bundler would have produced.
set -euo pipefail

cd "$(dirname "$0")"
src="logo-lock-2.icon"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT

# actool keys CFBundleIconName off the .icon basename, so it must be "Icon".
cp -R "$src" "$out/Icon.icon"
mkdir -p "$out/compiled"

actool "$out/Icon.icon" \
  --compile "$out/compiled" \
  --output-format human-readable-text \
  --notices --warnings \
  --output-partial-info-plist "$out/compiled/assetcatalog_generated_info.plist" \
  --app-icon Icon \
  --include-all-app-icons \
  --accent-color AccentColor \
  --enable-on-demand-resources NO \
  --development-region en \
  --target-device mac \
  --minimum-deployment-target 26.0 \
  --platform macosx

test -f "$out/compiled/Assets.car"
cp "$out/compiled/Assets.car" Assets.car
echo "Wrote $(pwd)/Assets.car"
