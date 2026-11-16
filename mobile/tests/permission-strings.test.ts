/* iOS permission prompts in every catalogue language (#1724): plugins/withPermissionStrings.js. */

import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

const require = createRequire(import.meta.url);
const plugin = require("../plugins/withPermissionStrings.js");
const appJson = require("../app.json");
const en = require("../../frontend/src/i18n/locales/en/mobile.json");

type PluginEntry = string | [string, Record<string, unknown>];

function pluginOptions(name: string): Record<string, unknown> {
  const entry = (appJson.expo.plugins as PluginEntry[]).find((p) => Array.isArray(p) && p[0] === name);
  assert.ok(Array.isArray(entry), `${name} is configured with options`);
  return entry[1];
}

test("the plugin is registered", () => {
  assert.ok((appJson.expo.plugins as PluginEntry[]).includes("./plugins/withPermissionStrings"));
});

test("every shipped language gets every prompt, in its own words", () => {
  const tables = plugin.permissionStrings();
  assert.deepEqual(Object.keys(tables).sort(), ["ar", "en", "es", "fr", "ru", "uk", "zh-Hans"]);
  for (const [lproj, strings] of Object.entries(tables) as [string, Record<string, string>][]) {
    assert.deepEqual(Object.keys(strings).sort(), Object.keys(plugin.KEYS).sort(), lproj);
    for (const [key, text] of Object.entries(strings)) {
      assert.ok(text.trim().length > 0, `${lproj} ${key}`);
      assert.ok(!text.includes("{{"), `${lproj} ${key} has no placeholder`);
      if (lproj !== "en") {
        assert.notEqual(text, tables.en[key], `${lproj} ${key} is translated, not English`);
      }
    }
  }
});

test("the English Info.plist values app.json sets are the en catalogue's", () => {
  const p = en.permissions;
  assert.equal(appJson.expo.ios.infoPlist.NSCameraUsageDescription, p.camera);
  assert.equal(pluginOptions("expo-camera").cameraPermission, p.camera);
  assert.equal(pluginOptions("expo-image-picker").cameraPermission, p.camera);
  assert.equal(pluginOptions("expo-image-picker").photosPermission, p.photoLibrary);
  assert.equal(pluginOptions("expo-media-library").photosPermission, p.photoLibrary);
  assert.equal(pluginOptions("expo-media-library").savePhotosPermission, p.photoLibraryAdd);
  assert.equal(pluginOptions("expo-secure-store").faceIDPermission, p.faceId);
});
