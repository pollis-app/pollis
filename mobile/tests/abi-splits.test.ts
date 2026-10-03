/*
 * plugins/withAbiSplits.js — optional per-ABI APK splits for the sideload release.
 *
 * Pinned: splits are off unless POLLIS_ABI_SPLITS=true (so no other build changes
 * shape), the split ABIs are exactly the ones pollis-core is built for in
 * modules/pollis-native/ubrn.config.yaml (an APK for any other ABI would install
 * and crash loading the core), there is no universal APK, and re-running
 * prebuild is idempotent.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const require = createRequire(import.meta.url);
const { addAbiSplits } = require("../plugins/withAbiSplits.js");

const TEMPLATE = `apply plugin: "com.android.application"

android {
    ndkVersion rootProject.ext.ndkVersion
    defaultConfig {
        applicationId 'com.pollis.mobile'
    }
}
`;

test("splits are opt-in and default off", () => {
  const out = addAbiSplits(TEMPLATE);
  assert.match(out, /System\.getenv\('POLLIS_ABI_SPLITS'\) \?: 'false'/);
  assert.match(out, /universalApk false/);
  assert.equal(out.split("splits {").length - 1, 1);
});

test("the split ABIs are exactly pollis-core's ubrn android targets", () => {
  const ubrn = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../modules/pollis-native/ubrn.config.yaml"), "utf8");
  const androidBlock = ubrn.split(/^ios:/m)[0];
  const targets = [...androidBlock.matchAll(/^\s+- ([a-z0-9_-]+)\s*$/gm)].map((m) => m[1]).sort();
  const out = addAbiSplits(TEMPLATE);
  const included = /include ([^\n]+)/.exec(out)![1].split(",").map((s) => s.trim().replace(/'/g, "")).sort();
  assert.deepEqual(included, targets);
});

test("re-running is a no-op", () => {
  const once = addAbiSplits(TEMPLATE);
  assert.equal(addAbiSplits(once), once);
});
