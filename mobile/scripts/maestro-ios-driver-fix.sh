#!/usr/bin/env bash
# Make Maestro's iOS driver start under Xcode 27 (idempotent; LOCAL MAC ONLY).
#
# Symptom: every iOS run dies with "iOS driver not ready in time, consider
# increasing timeout by configuring MAESTRO_DRIVER_STARTUP_TIMEOUT" — on every
# simulator, and raising the timeout does not help.
#
# Cause: Maestro 2.11.0's prebuilt driver bundle contains a unit-test class,
# SwipeRouteHandlerV2ClassificationTests, next to the real entry point
# (maestro_driver_iosUITests/testHttpServer), and its .xctestrun only skips
# ViewHierarchyHandlerTests — so xcodebuild runs the unit tests FIRST
# ('S' < 'm'). Under Xcode 27's XCTest the first one finishes, then the runner
# blocks forever in XCTCrashLogTracker.waitForPendingCrashlogs() and the HTTP
# server test that Maestro waits for never starts. (Seen in the driver log:
# ~/.maestro/tests/<run>/xctest_runner_*.log, or the run's --debug-output dir.)
#
# Fix: add that class to SkipTestIdentifiers in the simulator .xctestrun inside
# ~/.maestro/lib/maestro-ios-driver.jar. The original jar is kept beside it as
# maestro-ios-driver.jar.orig. Re-run this after reinstalling/upgrading Maestro
# (an upgrade restores the stock jar). Remove the script once a Maestro release
# skips the class itself.
#
# Usage: mobile/scripts/maestro-ios-driver-fix.sh [--revert]
set -euo pipefail

JAR="${MAESTRO_HOME:-$HOME/.maestro}/lib/maestro-ios-driver.jar"
ENTRY="driver-iPhoneSimulator/maestro-driver-ios-config.xctestrun"
SKIP="SwipeRouteHandlerV2ClassificationTests"
KEY=":maestro-driver-iosUITests:SkipTestIdentifiers"

[ -f "$JAR" ] || { echo "no Maestro driver jar at $JAR" >&2; exit 1; }

if [ "${1:-}" = "--revert" ]; then
  [ -f "$JAR.orig" ] || { echo "no backup at $JAR.orig" >&2; exit 1; }
  cp "$JAR.orig" "$JAR"; echo "restored $JAR from .orig"; exit 0
fi

if unzip -p "$JAR" "$ENTRY" | grep -q "<string>$SKIP</string>"; then
  echo "already patched: $JAR"; exit 0
fi

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
[ -f "$JAR.orig" ] || cp "$JAR" "$JAR.orig"
( cd "$WORK"
  unzip -q "$JAR" "$ENTRY"
  /usr/libexec/PlistBuddy -c "Add $KEY: string $SKIP" "$ENTRY"
  plutil -lint "$ENTRY" >/dev/null
  zip -q "$JAR" "$ENTRY" )
echo "patched $JAR (backup: $JAR.orig) — $SKIP is now skipped"
# Kill any runner left hanging by a previous failed attempt.
pkill -f 'xcodebuild test-without-building' 2>/dev/null || true
