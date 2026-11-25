#!/usr/bin/env bash
# Build and run the share extension's logic harness on macOS (#2100). No simulator, no Rust build:
# the UIKit-free Swift (model, core wrappers, intake, copy) against a stub core.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="$(cd "$here/.." && pwd)"
core_include="$(cd "$here/../../../../pollis-core/include" && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
xcrun clang -mmacosx-version-min=13 -I "$core_include" -c "$here/stub.c" -o "$out/stub.o"
xcrun swiftc -O -target arm64-apple-macos13 -Xcc -mmacosx-version-min=13 \
  -import-objc-header "$here/stub.h" -I "$core_include" \
  "$src/ShareCore.swift" "$src/ShareItems.swift" "$src/ShareModel.swift" "$src/ShareStrings.swift" \
  "$src/BiometricKey.swift" "$src/ShareIntake.swift" "$here/main.swift" "$out/stub.o" -o "$out/harness"
# The English strings the plugin generates, where Bundle.main of a command-line tool looks.
mkdir -p "$out/en.lproj"
node -e 'process.stdout.write(require(process.argv[1]).localizableStrings().en)' \
  "$src/../withShareExtension.js" > "$out/en.lproj/Localizable.strings"
"$out/harness"
