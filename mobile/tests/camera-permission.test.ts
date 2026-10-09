/*
 * The QR device-linking scanner (app/(auth)/link.tsx) needs the camera on both
 * platforms (#1255).
 *
 * Pinned: the config resolved exactly as `expo config --type introspect` does
 * (prebuild config + every plugin's mods, introspect-only, nothing written) keeps
 * android.permission.CAMERA in the AndroidManifest with no tools:node="remove",
 * and iOS carries the pairing NSCameraUsageDescription. Any plugin option of
 * `cameraPermission: false` (expo-image-picker turns that into a blocked CAMERA
 * on Android and a deleted usage string on iOS) fails this.
 *
 *   cd mobile && pnpm test
 */

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
const PAIRING_COPY = "Pollis needs your camera to scan the pairing QR code from your desktop.";

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
