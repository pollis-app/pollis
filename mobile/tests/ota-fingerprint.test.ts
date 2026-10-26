/*
 * One commit, one runtime version, wherever it is computed (#1427).
 *
 * A store build (macOS, after Gradle/Xcode ran) and the OTA job (Linux, fresh
 * `pnpm install`, no Gradle) must compute the same fingerprint, or updates
 * never reach store binaries. They did not: Gradle rewrites
 * @react-native-masked-view/masked-view's AndroidManifest.xml in node_modules,
 * and the npm ubrn bin leaves cargo output in its package dir. Pinned:
 *
 *   - the masked-view manifest hashes identically pristine and rewritten,
 *     and as the REWRITTEN bytes (what Play 1.0.14 was built from);
 *   - the hook buffers the file, so a chunk boundary cannot split the edit;
 *   - masked-view's build.gradle still makes exactly the edit mirrored here;
 *   - no other installed package's build.gradle rewrites its own sources;
 *   - the npm ubrn build output and Kotlin's `.kotlin/` build state are ignored;
 *   - the store-build ledger parses, and an update's reach is computed from it.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { parseStoreBuilds, reachOf } from "../scripts/ota/lib.ts";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const mobile = join(here, "..");
const fingerprint = require("../fingerprint.config.js");

const MANIFEST = "node_modules/@react-native-masked-view/masked-view/android/src/main/AndroidManifest.xml";
// The file as npm ships it, and as masked-view's build.gradle leaves it.
const PRISTINE = '<manifest package="org.reactnative.maskedview" xmlns:android="http://schemas.android.com/apk/res/android">\n</manifest>\n';
const REWRITTEN = '<manifest  xmlns:android="http://schemas.android.com/apk/res/android">\n</manifest>\n';

// Feed a file through the hook the way @expo/fingerprint streams it.
function streamThroughHook(filePath: string, bytes: Buffer, chunkSize: number): Buffer {
  const hook = fingerprint.fileHookTransform;
  const source = { type: "file", filePath };
  const out: Buffer[] = [];
  for (let i = 0; i < bytes.length; i += chunkSize) {
    const r = hook(source, bytes.subarray(i, i + chunkSize), false, "buffer");
    if (r != null) {
      out.push(Buffer.from(r));
    }
  }
  const last = hook(source, null, true, "utf8");
  if (last != null) {
    out.push(Buffer.from(last));
  }
  return Buffer.concat(out);
}

test("masked-view's manifest hashes as Gradle leaves it, pristine or already rewritten", () => {
  assert.equal(fingerprint.normaliseGradleRewrite(MANIFEST, PRISTINE), REWRITTEN);
  assert.equal(fingerprint.normaliseGradleRewrite(MANIFEST, REWRITTEN), REWRITTEN);
  // Any other file is untouched.
  assert.equal(fingerprint.normaliseGradleRewrite("node_modules/x/AndroidManifest.xml", PRISTINE), PRISTINE);
});

test("the hook buffers a rewritten file, so no chunk boundary splits the edit", () => {
  for (const size of [1, 7, 30, 1024]) {
    assert.equal(streamThroughHook(MANIFEST, Buffer.from(PRISTINE), size).toString("utf8"), REWRITTEN, `chunk size ${size}`);
    assert.equal(streamThroughHook(MANIFEST, Buffer.from(REWRITTEN), size).toString("utf8"), REWRITTEN, `chunk size ${size}`);
  }
  // Ordinary files stream through byte for byte.
  const other = Buffer.from(PRISTINE);
  assert.deepEqual(streamThroughHook("node_modules/x/AndroidManifest.xml", other, 7), other);
});

const nodeModules = join(mobile, "node_modules");
const installed = existsSync(join(nodeModules, "@react-native-masked-view", "masked-view"));

test("masked-view's build.gradle still makes exactly the edit fingerprint.config.js mirrors", { skip: !installed && "node_modules not installed" }, () => {
  const gradle = readFileSync(join(nodeModules, "@react-native-masked-view", "masked-view", "android", "build.gradle"), "utf8");
  assert.ok(gradle.includes('def PACKAGE_PROP = "package=\\"org.reactnative.maskedview\\""'), "PACKAGE_PROP changed: update GRADLE_REWRITTEN_FILES");
  assert.match(gradle, /manifestContent\.replaceAll\(\s*PACKAGE_PROP,\s*''\s*\)/);
  assert.match(gradle, /manifestOutFile\.write\(manifestContent\)/);
  // The installed file is one of the two forms this test knows.
  const onDisk = readFileSync(join(mobile, MANIFEST), "utf8");
  assert.ok(onDisk === PRISTINE || onDisk === REWRITTEN, `unexpected ${MANIFEST}:\n${onDisk}`);
});

// A package whose Gradle script writes into its own sources gives a store build
// (Gradle ran) and the OTA job (it did not) different bytes for one commit.
// Each one needs an entry in GRADLE_REWRITTEN_FILES or an ignore path.
test("no other installed package's build.gradle rewrites its own source files", { skip: !installed && "node_modules not installed" }, () => {
  const writers: string[] = [];
  const consider = (dir: string, name: string) => {
    const gradle = join(dir, "android", "build.gradle");
    if (!existsSync(gradle)) {
      return;
    }
    const text = readFileSync(gradle, "utf8");
    if (/file\("\$\{projectDir\}\/src\//.test(text) && /\.write\(|\.text\s*=|writeText\(/.test(text)) {
      writers.push(name);
    }
  };
  for (const entry of readdirSync(nodeModules)) {
    if (entry.startsWith(".")) {
      continue;
    }
    if (entry.startsWith("@")) {
      for (const sub of readdirSync(join(nodeModules, entry))) {
        consider(join(nodeModules, entry, sub), `${entry}/${sub}`);
      }
    } else {
      consider(join(nodeModules, entry), entry);
    }
  }
  const known = Object.keys(fingerprint.GRADLE_REWRITTEN_FILES).map((p: string) => p.split("/").slice(1, 3).join("/"));
  assert.deepEqual(writers.sort(), known.sort());
});

test("build output a tool leaves inside an installed package is not fingerprinted", () => {
  const ignored: string[] = fingerprint.ignorePaths;
  for (const p of fingerprint.NODE_MODULES_BUILD_OUTPUT) {
    assert.ok(ignored.includes(p), `${p} is not in ignorePaths`);
  }
  assert.ok(ignored.includes("node_modules/uniffi-bindgen-react-native/target/**/*"));
  assert.ok(ignored.includes("node_modules/uniffi-bindgen-react-native/Cargo.lock"));
  assert.ok(ignored.includes("node_modules/**/.kotlin/**/*"));
});

// #1429: the Kotlin Gradle plugin keeps `.kotlin/sessions/*.salive` in each
// included build's root for exactly as long as the build runs, and
// expo-updates bakes the runtime version mid-build. The sideload APKs hashed it
// (expo-updates-gradle-plugin is a fingerprinted dir); the OTA job never did.
// End to end through @expo/fingerprint, the way Gradle's bake step calls it.
const updatesPlugin = join(nodeModules, "expo-updates", "expo-updates-gradle-plugin");
test(
  "a Kotlin build-session file mid-build does not move the runtime version",
  { skip: !existsSync(updatesPlugin) && "node_modules not installed" },
  async () => {
    const { createFingerprintAsync } = require("expo/fingerprint");
    const options = { platforms: ["android"], ignorePaths: ["android/**/*", "ios/**/*"], silent: true };
    const kotlinDir = join(updatesPlugin, ".kotlin");
    const existed = existsSync(kotlinDir);
    const session = join(kotlinDir, "sessions", `kotlin-compiler-${process.pid}.salive`);
    const before = await createFingerprintAsync(mobile, options);
    mkdirSync(dirname(session), { recursive: true });
    writeFileSync(session, "");
    try {
      const during = await createFingerprintAsync(mobile, options);
      assert.equal(during.hash, before.hash);
    } finally {
      rmSync(existed ? session : kotlinDir, { recursive: true, force: true });
    }
  },
);

const ledger = readFileSync(join(mobile, "store", "runtime-versions.json"), "utf8");

test("the store-build ledger parses and records the Play 1.0.14 binary", () => {
  const builds = parseStoreBuilds(ledger);
  const play = builds.find((b) => b.channel === "play" && b.platform === "android" && b.version === "1.0.14");
  assert.equal(play?.runtimeVersion, "5466ba191d93be2e4fae2ca99c6ccdc2c964ffe4");
});

test("a malformed ledger entry is an error, not a silent mismatch", () => {
  const entry = { platform: "android", channel: "play", version: "1", commit: "c", runtimeVersion: "5466ba19" };
  assert.throws(() => parseStoreBuilds(JSON.stringify({ builds: [entry] })), /40 lowercase hex/);
  assert.throws(() => parseStoreBuilds(JSON.stringify({ builds: [{ ...entry, runtimeVersion: "a".repeat(40), platform: "web" }] })), /platform/);
  assert.throws(() => parseStoreBuilds(JSON.stringify({ builds: [{ ...entry, runtimeVersion: "a".repeat(40), commit: "" }] })), /commit/);
  assert.throws(() => parseStoreBuilds("{}"), /builds/);
});

test("an update's reach is the recorded binaries with its runtime version", () => {
  const builds = parseStoreBuilds(ledger);
  const reach = reachOf({ android: "5466ba191d93be2e4fae2ca99c6ccdc2c964ffe4", ios: "b".repeat(40) }, builds);
  assert.deepEqual(
    reach.map((r) => [r.platform, r.binaries.map((b) => b.channel)]),
    [
      ["ios", []],
      // Sideload 1.0.14 via its ledger alias (#1446; tests/ota-aliases.test.ts).
      ["android", ["play", "sideload"]],
    ],
  );
  // The value the OTA job used to compute for 1.0.14's native layer reached nothing.
  assert.equal(reachOf({ android: "35e62c3e7d8d032e4ac83ac761ca268e6e4f2d1b" }, builds)[0].binaries.length, 0);
});

// #1429: a baked runtime version the OTA job does not compute strands the
// binary, so both Gradle builds in CI refuse one rather than warn about it.
test("CI fails any APK whose baked runtime version the OTA job would not compute", () => {
  const workflows = join(mobile, "..", ".github", "workflows");
  const checks: Array<[string, RegExp]> = [
    ["mobile-apk-release.yml", /- name: Check the baked runtime version and record its sources\n([\s\S]*?)(?=\n {6}- name:)/],
    ["mobile-core-check.yml", /- name: Baked runtime version equals the OTA job's\n([\s\S]*?)(?=\n {6}- name:)/],
  ];
  for (const [file, stepRe] of checks) {
    const step = stepRe.exec(readFileSync(join(workflows, file), "utf8"))?.[1];
    assert.ok(step, `${file}: the runtime-version check step is missing`);
    assert.doesNotMatch(step, /continue-on-error/, `${file}: the check must fail the job`);
    assert.match(step, /fingerprint:generate --platform android/, file);
    assert.match(step, /assets\/fingerprint/, file);
    assert.match(step, /if \[ "\$baked" != "\$computed" \]; then[\s\S]*?exit 1/, file);
  }
});
