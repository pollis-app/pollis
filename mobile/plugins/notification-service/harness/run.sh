#!/usr/bin/env bash
# Build and run the NSE rewrite harness on macOS (#1599). No simulator, no Rust build.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="$(cd "$here/.." && pwd)"
core_include="$(cd "$here/../../../../pollis-core/include" && pwd)"
out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
xcrun clang -mmacosx-version-min=13 -c "$here/stub.c" -o "$out/stub.o"
xcrun swiftc -O -target arm64-apple-macos13 -Xcc -mmacosx-version-min=13 \
  -import-objc-header "$src/PollisNotificationService-Bridging-Header.h" -I "$core_include" \
  "$src/NotificationService.swift" "$src/LetterAvatar.swift" "$here/main.swift" "$out/stub.o" -o "$out/harness"
# The English strings the plugin generates, where Bundle.main of a command-line tool looks.
mkdir -p "$out/en.lproj"
node -e 'process.stdout.write(require(process.argv[1]).localizableStrings().en)' \
  "$src/../withNotificationServiceExtension.js" > "$out/en.lproj/Localizable.strings"
"$out/harness"
