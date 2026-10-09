/*
 * The app side of OTA updates (#1250): app.config.js's `updates` block and the
 * fingerprint the runtime version is computed from (fingerprint.config.js).
 *
 * Pinned:
 *   - only POLLIS_OTA=production switches updates on; every other build
 *     (dev clients, Maestro's api-dev Release builds, CI's debug APK) has
 *     `enabled: false` and NO url, so it cannot ask the update server anything;
 *   - a prod build carries the code-signing certificate + metadata, refuses to
 *     configure without the certificate, and refuses an api-dev DS;
 *   - checks on launch in the background (ON_LOAD, fallbackToCacheTimeout 0);
 *   - the runtime version policy is `fingerprint`, and the fingerprint covers
 *     pollis-core + every crate it reaches, ignores ubrn's generated output,
 *     hashes the OTA certificate, and ignores the version fields and the
 *     updates block;
 *   - the prod builds (sideload APK workflow) set POLLIS_OTA=production, and
 *     the OTA release's signing job runs in the protected environment.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { MANIFEST_URL, assertCodeSigningCertificate } from "../scripts/ota/lib.ts";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const mobile = join(here, "..");
const repo = join(mobile, "..");
const appConfig = require("../app.config.js");
const fingerprint = require("../fingerprint.config.js");
const { updatesConfigFor, OTA } = appConfig;

function evaluate(env: Record<string, string | undefined>) {
  const saved = { ...process.env };
  try {
    for (const k of ["POLLIS_OTA", "EXPO_PUBLIC_POLLIS_DELIVERY_URL"]) {
      delete process.env[k];
    }
    Object.assign(process.env, env);
    const appJson = JSON.parse(readFileSync(join(mobile, "app.json"), "utf8"));
    return appConfig({ config: appJson.expo });
  } finally {
    process.env = saved;
  }
}

test("a build without POLLIS_OTA=production has updates off and no URL at all", () => {
  for (const env of [{}, { POLLIS_OTA: "" }, { POLLIS_OTA: "off" }, { EXPO_PUBLIC_POLLIS_DELIVERY_URL: "https://api-dev.pollis.com" }]) {
    const config = evaluate(env);
    assert.deepEqual(config.updates, { enabled: false }, JSON.stringify(env));
  }
});

test("update requests carry no per-install identifier: EAS-Client-ID is a constant for everyone", () => {
  const u = updatesConfigFor({ POLLIS_OTA: "production" }, true);
  assert.equal(u.requestHeaders["EAS-Client-ID"], OTA.OTA_NEUTRAL_CLIENT_ID);
  assert.match(OTA.OTA_NEUTRAL_CLIENT_ID, /^0{8}-0{4}-0{4}-0{4}-0{12}$/);
  assert.equal(u.requestHeaders["Expo-Fatal-Error"], "");
  // The override only works because both native downloaders apply
  // config.requestHeaders AFTER their own EAS-Client-ID, on manifest and asset
  // requests alike. Pin that ordering in the installed expo-updates, so an
  // upgrade that moves it fails here instead of silently re-identifying installs.
  const ios = readFileSync(join(mobile, "node_modules", "expo-updates", "ios", "EXUpdates", "AppLoader", "FileDownloader.swift"), "utf8");
  for (const fn of ["private func setHTTPHeaderFields(", "private func setManifestHTTPHeaderFields("]) {
    const body = ios.slice(ios.indexOf(fn), ios.indexOf("\n  }\n", ios.indexOf(fn)));
    assert.ok(body.indexOf('forHTTPHeaderField: "EAS-Client-ID"') > 0, fn);
    assert.ok(body.indexOf("for (key, value) in config.requestHeaders") > body.indexOf('forHTTPHeaderField: "EAS-Client-ID"'), fn);
  }
  const android = readFileSync(
    join(mobile, "node_modules", "expo-updates", "android", "src", "main", "java", "expo", "modules", "updates", "loader", "FileDownloader.kt"),
    "utf8",
  );
  for (const fn of ["fun createRequestForAsset(", "fun createRequestForRemoteUpdate("]) {
    const start = android.indexOf(fn);
    const body = android.slice(start, android.indexOf(".build()", start));
    assert.ok(body.indexOf('.header("EAS-Client-ID", easClientID)') > 0, fn);
    assert.ok(body.indexOf("for ((key, value) in configuration.requestHeaders)") > body.indexOf('.header("EAS-Client-ID", easClientID)'), fn);
  }
});

test("an unknown POLLIS_OTA value is an error, not a silent off", () => {
  assert.throws(() => updatesConfigFor({ POLLIS_OTA: "prod" }, true), /must be "production"/);
  assert.throws(() => updatesConfigFor({ POLLIS_OTA: "staging" }, true), /must be "production"/);
});

test("a prod build refuses to configure without the code-signing certificate", () => {
  assert.throws(() => updatesConfigFor({ POLLIS_OTA: "production" }, false), /ota-code-signing\.pem is missing/);
});

test("a prod build refuses an api-dev Delivery Service", () => {
  assert.throws(
    () => updatesConfigFor({ POLLIS_OTA: "production", EXPO_PUBLIC_POLLIS_DELIVERY_URL: "https://api-dev.pollis.com" }, true),
    /needs EXPO_PUBLIC_POLLIS_DELIVERY_URL/,
  );
});

test("a prod build is code-signed, checks on load in the background, and names the prod channel", () => {
  const u = updatesConfigFor({ POLLIS_OTA: "production", EXPO_PUBLIC_POLLIS_DELIVERY_URL: "https://api.pollis.com" }, true);
  assert.deepEqual(u, {
    enabled: true,
    url: "https://api.pollis.com/updates/api/manifest",
    checkAutomatically: "ON_LOAD",
    fallbackToCacheTimeout: 0,
    enableBsdiffPatchSupport: false,
    codeSigningCertificate: "./store/ota-code-signing.pem",
    codeSigningMetadata: { keyid: "main", alg: "rsa-v1_5-sha256" },
    requestHeaders: {
      "expo-channel-name": "production",
      "EAS-Client-ID": "00000000-0000-0000-0000-000000000000",
      "Expo-Fatal-Error": "",
    },
  });
  // The URL a prod build asks is the URL the pipeline publishes for.
  assert.equal(u.url, MANIFEST_URL);
});

test("app.config.js evaluates the committed certificate when one exists", () => {
  const certPath = join(mobile, OTA.OTA_CERT_PATH);
  if (!existsSync(certPath)) {
    // Not generated yet (owner step): a prod build must then fail loudly.
    assert.throws(() => evaluate({ POLLIS_OTA: "production" }), /missing/);
    return;
  }
  const config = evaluate({ POLLIS_OTA: "production" });
  assert.equal(config.updates.enabled, true);
  assert.equal(config.updates.codeSigningCertificate, OTA.OTA_CERT_PATH);
  assert.doesNotThrow(() => assertCodeSigningCertificate(readFileSync(certPath, "utf8")));
  assert.doesNotMatch(readFileSync(certPath, "utf8"), /PRIVATE KEY/);
});

test("no private key is anywhere in mobile/store", () => {
  const dir = join(mobile, "store");
  for (const name of require("node:fs").readdirSync(dir) as string[]) {
    assert.doesNotMatch(readFileSync(join(dir, name), "utf8"), /PRIVATE KEY/, name);
  }
});

test("the runtime version policy is the native fingerprint", () => {
  const appJson = JSON.parse(readFileSync(join(mobile, "app.json"), "utf8"));
  assert.deepEqual(appJson.expo.runtimeVersion, { policy: "fingerprint" });
  assert.equal(appJson.expo.updates, undefined, "the updates block belongs to app.config.js, which gates it");
});

test("the fingerprint covers pollis-core and every crate it reaches by path", () => {
  const closure: string[] = fingerprint.rustCrateClosure();
  assert.ok(closure.includes("pollis-core"));
  const core = readFileSync(join(repo, "pollis-core", "Cargo.toml"), "utf8");
  for (const m of core.matchAll(/path\s*=\s*"\.\.\/([A-Za-z0-9_.-]+)"/g)) {
    assert.ok(closure.includes(m[1]), `${m[1]} is a pollis-core path dependency`);
  }
  const digest: string = fingerprint.rustCoreDigest().contents;
  assert.match(digest, / {2}pollis-core\/src\/bridge\.rs\n/);
  assert.match(digest, / {2}Cargo\.lock\n/);
  assert.match(digest, / {2}rust-toolchain\.toml\n/);
  assert.match(digest, /^uniffi-bindgen-react-native /m);
  assert.doesNotMatch(digest, /\/tests\//);
  const extra = fingerprint.extraSources.find((s: { id?: string }) => s.id === "pollis-core");
  assert.equal(extra.contents, digest);
});

test("the fingerprint ignores everything ubrn generates (mobile/.gitignore lists it)", () => {
  const ignored: string[] = fingerprint.GENERATED_NATIVE_BRIDGE;
  const gitignore = readFileSync(join(mobile, ".gitignore"), "utf8")
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("modules/pollis-native/"));
  assert.ok(gitignore.length > 0);
  for (const entry of gitignore) {
    const stem = entry.replace(/\/$/, "");
    assert.ok(
      ignored.some((p) => p === stem || p.startsWith(`${stem}/`)),
      `${entry} is generated but would be fingerprinted`,
    );
  }
});

test("the fingerprint hashes the OTA certificate, so a rotation is a new runtime version", () => {
  const extra = fingerprint.extraSources.find((s: { id?: string }) => s.id === "ota-code-signing-certificate");
  const certPath = join(mobile, OTA.OTA_CERT_PATH);
  const expected = existsSync(certPath)
    ? `${createHash("sha256").update(readFileSync(certPath)).digest("hex")}\n`
    : "absent\n";
  assert.equal(extra.contents, expected);
});

test("the fingerprint skips version fields and the updates block", () => {
  assert.ok(fingerprint.sourceSkips.includes("ExpoConfigVersions"));
  const hook = fingerprint.fileHookTransform;
  const config = JSON.stringify({ name: "Pollis", plugins: ["x"], updates: { enabled: true, url: "u" } });
  assert.deepEqual(JSON.parse(hook({ type: "contents", id: "expoConfig" }, config, true, "utf8")), { name: "Pollis", plugins: ["x"] });
  // Every other source passes through untouched.
  assert.equal(hook({ type: "file", filePath: "a" }, "bytes", true, "utf8"), "bytes");
  assert.equal(hook({ type: "contents", id: "pollis-core" }, "digest", true, "utf8"), "digest");
});

test("prod builds switch OTA on; the signing job is behind the protected environment", () => {
  const apk = readFileSync(join(repo, ".github", "workflows", "mobile-apk-release.yml"), "utf8");
  assert.match(apk, /POLLIS_OTA: production/);
  const ota = readFileSync(join(repo, ".github", "workflows", "mobile-ota-release.yml"), "utf8");
  // The key is referenced exactly once, in the job that declares the environment.
  const uses = ota.split("secrets.OTA_CODE_SIGNING_KEY").length - 1;
  assert.equal(uses, 1);
  const signJob = /\n {2}sign:\n([\s\S]*?)(?=\n {2}[a-z-]+:\n)/.exec(ota)?.[1] ?? "";
  assert.match(signJob, /environment: ota-signing/);
  assert.match(signJob, /secrets\.OTA_CODE_SIGNING_KEY/);
});
