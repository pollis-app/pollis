/* The Android build blocks the permissions libraries add but the app never uses, and ships R8-shrunk. */

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

// Network, QR pairing, push, and saving media on Android 7–9.
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
