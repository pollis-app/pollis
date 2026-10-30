/* #1599: the iOS Notification Service Extension plugin and the push capability the app advertises. */

import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import { pushCapsFor } from "../lib/push/caps.ts";

const require = createRequire(import.meta.url);
const plugin = require("../plugins/withNotificationServiceExtension.js");
const appJson = require("../app.json");

const config = { ios: { bundleIdentifier: "com.pollis.ios", buildNumber: "1002300" }, version: "1.0.23" };
const ids = plugin.identifiers(config, { teamId: "8JT3WH45NZ" });

test("the identifiers match what the developer portal must hold", () => {
  assert.deepEqual(ids, {
    app: "com.pollis.ios",
    extension: "com.pollis.ios.NotificationService",
    group: "group.com.pollis.ios",
    keychain: "8JT3WH45NZ.com.pollis.shared",
  });
  assert.throws(() => plugin.identifiers(config, {}), /teamId/);
});

test("app.json registers the plugin with the Pollis Inc. team", () => {
  const entry = appJson.expo.plugins.find(
    (p: unknown) => Array.isArray(p) && p[0] === "./plugins/withNotificationServiceExtension",
  );
  assert.ok(entry, "plugin registered");
  assert.equal(entry[1].teamId, "8JT3WH45NZ");
});

test("both targets share the App Group and put the shared keychain group first", () => {
  const out = plugin.sharedEntitlements(
    { "aps-environment": "development", "keychain-access-groups": ["X.other"] },
    ids,
  );
  assert.equal(out["aps-environment"], "development");
  assert.deepEqual(out["com.apple.security.application-groups"], ["group.com.pollis.ios"]);
  assert.deepEqual(out["keychain-access-groups"], ["8JT3WH45NZ.com.pollis.shared", "X.other"]);
  // No unexpanded build variable: the store archive is unsigned and step 4 signs from the raw file.
  assert.ok(!JSON.stringify(out).includes("$("));
});

test("the extension's versions equal the app's (App Store Connect rejects a mismatch)", () => {
  const info = plugin.extensionInfoPlist(config, ids);
  assert.equal(info.CFBundleShortVersionString, "1.0.23");
  assert.equal(info.CFBundleVersion, "1002300");
  assert.equal(info.NSExtension.NSExtensionPointIdentifier, "com.apple.usernotifications.service");
  assert.equal(info.PollisAppGroup, "group.com.pollis.ios");
  assert.equal(info.PollisKeychainAccessGroup, "8JT3WH45NZ.com.pollis.shared");
});

test("every locale gets every string, with positional placeholders", () => {
  const tables = plugin.localizableStrings();
  assert.deepEqual(Object.keys(tables).sort(), ["ar", "en", "es", "fr", "ru", "uk", "zh-Hans"]);
  for (const [lproj, text] of Object.entries(tables) as [string, string][]) {
    for (const key of ["genericTitle", "genericBody", "nameOnlyBody", "groupTitle", "attachment", "image", "video", "voice", "sensitive"]) {
      assert.match(text, new RegExp(`^"${key}" = ".+";$`, "m"), `${lproj} ${key}`);
    }
    assert.match(text, /"groupTitle" = "[^"]*%1\$@[^"]*%2\$@[^"]*";/, `${lproj} groupTitle`);
    assert.ok(!text.includes("{{"), `${lproj} has no i18next placeholder left`);
  }
});

test("the Swift reads exactly the keys the plugin writes", () => {
  const swift = fs.readFileSync(
    path.join(import.meta.dirname, "../plugins/notification-service/NotificationService.swift"),
    "utf8",
  );
  const used = [...swift.matchAll(/NSLocalizedString\("(\w+)"/g)].map((m) => m[1]).sort();
  const written = [...plugin.localizableStrings().en.matchAll(/^"(\w+)" =/gm)].map((m) => m[1]).sort();
  assert.deepEqual(used, written);
  assert.match(swift, /forInfoDictionaryKey: "PollisAppGroup"/);
});

test("POLLIS_IOS_NSE=off leaves the config alone", () => {
  assert.equal(plugin.isEnabled({ POLLIS_IOS_NSE: "off" }), false);
  assert.equal(plugin.isEnabled({}), true);
});

test("a binary advertises rich only when its own embedded config says it has the extension", () => {
  assert.deepEqual(pushCapsFor({ richPush: { ios: true } }, "ios"), ["rich"]);
  // An OTA bundle on an Android binary without a rich handler must not claim it.
  assert.deepEqual(pushCapsFor({ richPush: { ios: true } }, "android"), []);
  assert.deepEqual(pushCapsFor({}, "ios"), []);
  assert.deepEqual(pushCapsFor(null, "ios"), []);
  assert.deepEqual(pushCapsFor({ richPush: { ios: "yes" } }, "ios"), []);
});
