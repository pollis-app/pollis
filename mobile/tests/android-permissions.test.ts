/*
 * The Android build asks only for what the app uses, and ships shrunk.
 *
 * Play reported 36 permissions on 1.0.11. Most arrived through libraries the
 * app does not use: the LiveKit/WebRTC packages (installed for voice, which is
 * deliberately not wired on mobile — #343) bring SYSTEM_ALERT_WINDOW,
 * FOREGROUND_SERVICE, Bluetooth and audio-settings permissions;
 * expo-secure-store brings the biometric ones (the app never requests
 * biometric auth); and a launcher-badge library brings sixteen vendor badge
 * permissions (the app never sets a badge count). FOREGROUND_SERVICE without a
 * declared service type and SYSTEM_ALERT_WINDOW are the kind Play review flags.
 *
 * Pinned: every one of those is in `android.blockedPermissions`, none of the
 * permissions the app does need is, and R8 + resource shrinking stay on for
 * release builds (Play: "DEX code optimization: Low", 65.5 MB of DEX).
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const app = JSON.parse(readFileSync(join(root, "app.json"), "utf8")).expo;
const blocked: string[] = app.android.blockedPermissions;

const MUST_BLOCK = [
  "android.permission.SYSTEM_ALERT_WINDOW",
  "android.permission.FOREGROUND_SERVICE",
  "android.permission.BLUETOOTH",
  "android.permission.BLUETOOTH_ADMIN",
  "android.permission.MODIFY_AUDIO_SETTINGS",
  "android.permission.USE_FINGERPRINT",
  "android.permission.USE_BIOMETRIC",
  "android.permission.DUMP",
  "android.permission.READ_APP_BADGE",
  "com.sec.android.provider.badge.permission.READ",
  "com.sec.android.provider.badge.permission.WRITE",
  "com.huawei.android.launcher.permission.CHANGE_BADGE",
  "me.everything.badger.permission.BADGE_COUNT_WRITE",
];

// What the app actually uses: network, QR pairing, push, and saving media to
// the library on Android 7–9 (WRITE_EXTERNAL_STORAGE before scoped storage).
const MUST_KEEP = [
  "android.permission.INTERNET",
  "android.permission.ACCESS_NETWORK_STATE",
  "android.permission.CAMERA",
  "android.permission.POST_NOTIFICATIONS",
  "android.permission.WAKE_LOCK",
  "android.permission.RECEIVE_BOOT_COMPLETED",
  "android.permission.VIBRATE",
  "android.permission.WRITE_EXTERNAL_STORAGE",
  "android.permission.READ_EXTERNAL_STORAGE",
];

test("permissions the app never uses are blocked", () => {
  for (const p of MUST_BLOCK) {
    assert.ok(blocked.includes(p), `${p} must be in android.blockedPermissions`);
  }
});

test("permissions the app needs are never blocked", () => {
  for (const p of MUST_KEEP) {
    assert.ok(!blocked.includes(p), `${p} is needed and must not be blocked`);
  }
});

test("release builds are minified and resource-shrunk", () => {
  const plugin = app.plugins.find((p: unknown) => Array.isArray(p) && p[0] === "expo-build-properties");
  assert.ok(plugin, "expo-build-properties is configured");
  assert.equal(plugin[1].android.enableMinifyInReleaseBuilds, true);
  assert.equal(plugin[1].android.enableShrinkResourcesInReleaseBuilds, true);
});
