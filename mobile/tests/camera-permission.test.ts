/* The QR scanner needs the camera on both platforms (#1255); `cameraPermission: false` anywhere fails this. */

import test from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const projectRoot = join(dirname(fileURLToPath(import.meta.url)), "..");
const { getPrebuildConfigAsync } = require("@expo/prebuild-config");
const { compileModsAsync } = require("@expo/config-plugins/build/plugins/mod-compiler.js");

const CAMERA = "android.permission.CAMERA";
const PAIRING_COPY = "Pollis needs your camera to scan sign-in QR codes from your other devices.";

type UsesPermission = { $: Record<string, string> };

async function introspect() {
  const config = await getPrebuildConfigAsync(projectRoot, { platforms: ["ios", "android"] });
  await compileModsAsync(config.exp, {
    projectRoot,
    introspect: true,
    platforms: ["ios", "android"],
    assertMissingModProviders: false,
  });
  return config.exp;
}

test("Android keeps CAMERA and iOS has the pairing camera string", async () => {
  const exp = await introspect();

  assert.ok(!(exp.android?.blockedPermissions ?? []).includes(CAMERA), "CAMERA must not be a blocked permission");

  const manifest = exp._internal.modResults.android.manifest.manifest;
  const camera = (manifest["uses-permission"] as UsesPermission[]).filter((p) => p.$["android:name"] === CAMERA);
  assert.equal(camera.length, 1, "AndroidManifest declares CAMERA exactly once");
  assert.notEqual(camera[0].$["tools:node"], "remove", "CAMERA must not be stripped by a tools:node=remove rule");

  const infoPlist = exp._internal.modResults.ios.infoPlist;
  assert.equal(infoPlist.NSCameraUsageDescription, PAIRING_COPY);
});
