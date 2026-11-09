/* Biometric unlock (#1685): where the key may live, when it is offered, what a read means. */
import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  classifyRead,
  deviceKeyName,
  deviceUnlockSupported,
  iosAccessGroup,
  offeredKeyName,
  shouldOffer,
  showSwitch,
} from "../lib/deviceUnlockRules.ts";
import { afterPinUnlock, pinScreenView } from "../lib/pinStage.ts";

const require = createRequire(import.meta.url);
const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const deviceUnlockPlugin = require("../plugins/withDeviceUnlock.js");
const nsePlugin = require("../plugins/withNotificationServiceExtension.js");
const app = JSON.parse(readFileSync(join(root, "app.json"), "utf8")).expo;

const GROUP = "8JT3WH45NZ.com.pollis.app";
const extra = { deviceUnlock: { iosAccessGroup: GROUP } };

test("the iOS key is only ever stored in the app-only group the binary names", () => {
  assert.equal(iosAccessGroup(extra), GROUP);
  // No group (an OTA bundle on an older binary): off, never the default group.
  assert.equal(iosAccessGroup({}), null);
  assert.equal(iosAccessGroup(null), null);
  assert.equal(iosAccessGroup({ deviceUnlock: { iosAccessGroup: "" } }), null);
  // The extension's group, or anything not team-prefixed, is refused.
  assert.equal(iosAccessGroup({ deviceUnlock: { iosAccessGroup: "8JT3WH45NZ.com.pollis.shared" } }), null);
  assert.equal(iosAccessGroup({ deviceUnlock: { iosAccessGroup: "$(AppIdentifierPrefix)com.pollis.app" } }), null);
  assert.equal(iosAccessGroup({ deviceUnlock: { iosAccessGroup: "com.pollis.app" } }), null);
});

test("supported only with enrolled strong biometrics, and on iOS only with the app-only group", () => {
  assert.equal(deviceUnlockSupported("ios", extra, true), true);
  assert.equal(deviceUnlockSupported("ios", {}, true), false);
  assert.equal(deviceUnlockSupported("ios", extra, false), false);
  assert.equal(deviceUnlockSupported("android", {}, true), true);
  assert.equal(deviceUnlockSupported("android", {}, false), false);
  assert.equal(deviceUnlockSupported("web", extra, true), false);
});

test("a read that throws is a cancel (keypad), a missing value is an invalidated key (re-arm)", () => {
  assert.deepEqual(classifyRead("abc", false), { kind: "key", key: "abc" });
  assert.deepEqual(classifyRead(null, false), { kind: "invalidated" });
  assert.deepEqual(classifyRead("", false), { kind: "invalidated" });
  assert.deepEqual(classifyRead(null, true), { kind: "cancelled" });
  assert.deepEqual(classifyRead("abc", true), { kind: "cancelled" });
});

test("off by default: offered once, only where it works, never when already on", () => {
  assert.equal(shouldOffer(true, false, false), true);
  assert.equal(shouldOffer(true, false, true), false);
  assert.equal(shouldOffer(true, true, false), false);
  assert.equal(shouldOffer(false, false, false), false);
  // The switch hides where it cannot work, unless it is on (so it can be turned off).
  assert.equal(showSwitch(false, false), false);
  assert.equal(showSwitch(false, true), true);
  assert.equal(showSwitch(true, false), true);
});

test("after a PIN unlock: re-arm a lost key, else the one-time offer, else straight in", () => {
  const base = { needsRearm: false, supported: true, enabled: false, offered: false };
  assert.equal(afterPinUnlock(base), "offer");
  assert.equal(afterPinUnlock({ ...base, offered: true }), "continue");
  assert.equal(afterPinUnlock({ ...base, enabled: true }), "continue");
  assert.equal(afterPinUnlock({ ...base, enabled: true, needsRearm: true }), "rearm");
  // Turned off meanwhile: nothing to re-arm, and the offer was already made.
  assert.equal(afterPinUnlock({ ...base, needsRearm: true, offered: true }), "continue");
  assert.equal(afterPinUnlock({ ...base, supported: false, needsRearm: true, enabled: true }), "continue");
});

test("the biometric stage keeps the loader up under the system prompt", () => {
  assert.equal(pinScreenView("biometric", false, false), "biometric");
  assert.equal(pinScreenView("unlock", false, false), "keypad");
});

test("key names stay inside SecureStore's alphabet and apart per user", () => {
  assert.equal(deviceKeyName("01HXYZ"), "duk.01HXYZ");
  assert.equal(offeredKeyName("01HXYZ"), "duk.01HXYZ.offered");
  assert.notEqual(deviceKeyName("a"), deviceKeyName("b"));
  assert.equal(deviceKeyName("a/b"), null);
  assert.equal(offeredKeyName(""), null);
});

test("the app gets the app-only group; the extension never does; the shared group stays the default", () => {
  const ids = nsePlugin.identifiers({ ios: { bundleIdentifier: "com.pollis.ios" } }, { teamId: "8JT3WH45NZ" });
  const group = deviceUnlockPlugin.appOnlyGroup({ teamId: "8JT3WH45NZ" });
  assert.equal(group, GROUP);
  // Either plugin order: shared first (the default for new items), app-only present once.
  const a = nsePlugin.sharedEntitlements(deviceUnlockPlugin.appEntitlements({}, group), ids);
  const b = deviceUnlockPlugin.appEntitlements(nsePlugin.sharedEntitlements({}, ids), group);
  for (const ent of [a, b]) {
    assert.equal(ent["keychain-access-groups"][0], "8JT3WH45NZ.com.pollis.shared");
    assert.equal(ent["keychain-access-groups"].filter((g: string) => g === GROUP).length, 1);
  }
  // The extension's entitlements are built from nothing but the shared ids.
  const extension = nsePlugin.sharedEntitlements({}, ids);
  assert.deepEqual(extension["keychain-access-groups"], ["8JT3WH45NZ.com.pollis.shared"]);
  assert.throws(() => deviceUnlockPlugin.appOnlyGroup({ teamId: "nope" }));
});

test("app.json wires the plugin, the Face ID string and the Android biometric permissions", () => {
  const plugin = app.plugins.find((p: unknown) => Array.isArray(p) && p[0] === "./plugins/withDeviceUnlock");
  assert.ok(plugin, "withDeviceUnlock is configured");
  assert.equal(plugin[1].teamId, "8JT3WH45NZ");
  const secureStore = app.plugins.find((p: unknown) => Array.isArray(p) && p[0] === "expo-secure-store");
  assert.ok(secureStore?.[1]?.faceIDPermission, "NSFaceIDUsageDescription is set (expo-secure-store refuses requireAuthentication without it)");
  for (const p of ["android.permission.USE_BIOMETRIC", "android.permission.USE_FINGERPRINT"]) {
    assert.ok(!app.android.blockedPermissions.includes(p), `${p} must not be blocked`);
  }
});

test("the extension can never be handed the device key: lib/deviceUnlock always passes the app-only group", () => {
  const src = readFileSync(join(root, "lib/deviceUnlock.ts"), "utf8");
  assert.match(src, /options\.accessGroup = group/);
  assert.match(src, /if \(!group\) \{\s*return null;/);
  assert.match(src, /requireAuthentication: true/);
  assert.match(src, /WHEN_PASSCODE_SET_THIS_DEVICE_ONLY/);
});
