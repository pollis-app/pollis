/* #1599 Android rich push: the token advertises "rich", title-less pushes are never presented by JS,
   expo's FCM service is replaced by ours, and the native copy matches the shared catalogues. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { pushCapsFor, RICH_PUSH_CAP } from "../lib/push/caps.ts";
import { isTitleless } from "../lib/push/presentation.ts";

const require = createRequire(import.meta.url);
const { removeExpoMessagingService, EXPO_SERVICE } = require("../plugins/withRichPush.js");
const HERE = dirname(fileURLToPath(import.meta.url));
const MOBILE = join(HERE, "..");
const ROOT = join(MOBILE, "..");

test("rich is asked for only when the embedded config marks this platform's native handler", () => {
  assert.deepEqual(pushCapsFor({ richPush: { android: true } }, "android"), [RICH_PUSH_CAP]);
  assert.deepEqual(pushCapsFor({ richPush: { android: true } }, "ios"), []);
  assert.deepEqual(pushCapsFor({ richPush: { android: "true" } }, "android"), []);
  assert.deepEqual(pushCapsFor({}, "android"), []);
  assert.deepEqual(pushCapsFor(null, "android"), []);
});

test("the token registration sends caps from the embedded config", () => {
  const src = readFileSync(join(MOBILE, "lib/push/index.ts"), "utf8");
  assert.match(src, /caps: pushCapsFor\(embeddedExtra\(\), Platform\.OS\)/);
});

test("withRichPush marks the binary rich-capable on Android without dropping other extra", () => {
  const { markRichPush } = require("../plugins/withRichPush.js");
  const config = markRichPush({ extra: { installChannel: "store", richPush: { ios: true } } });
  assert.deepEqual(config.extra.richPush, { ios: true, android: true });
  assert.equal(config.extra.installChannel, "store");
});

test("a title-less push is never presented by the JS handler; a normal one is", () => {
  assert.equal(isTitleless({ title: null, body: null }), true);
  assert.equal(isTitleless({ title: "", body: "" }), true);
  assert.equal(isTitleless({ title: "New message", body: "You have a new message" }), false);
  assert.equal(isTitleless({ title: null, body: "x" }), false);
});

test("withRichPush removes expo's FCM service from the app manifest, idempotently", () => {
  const manifest = { $: {}, application: [{ $: {}, activity: [] }] };
  removeExpoMessagingService(manifest);
  removeExpoMessagingService(manifest);
  const services = (manifest.application[0] as { service?: Array<{ $: Record<string, string> }> }).service ?? [];
  assert.equal(services.length, 1);
  assert.equal(services[0].$["android:name"], EXPO_SERVICE);
  assert.equal(services[0].$["tools:node"], "remove");
  assert.equal((manifest.$ as Record<string, string>)["xmlns:tools"], "http://schemas.android.com/tools");
});

test("the plugin is registered and our service is declared in pollis-native's manifest", () => {
  const app = JSON.parse(readFileSync(join(MOBILE, "app.json"), "utf8"));
  assert.ok(app.expo.plugins.includes("./plugins/withRichPush"));
  const manifest = readFileSync(
    join(MOBILE, "modules/pollis-native/android/src/main/AndroidManifestNew.xml"),
    "utf8",
  );
  assert.match(manifest, /com\.pollisnative\.push\.PollisMessagingService/);
  assert.match(manifest, /com\.google\.firebase\.MESSAGING_EVENT/);
});

test("the subclassed expo service is still the class name the plugin removes", () => {
  const kt = readFileSync(
    join(MOBILE, "modules/pollis-native/android/src/main/java/com/pollisnative/push/PollisMessagingService.kt"),
    "utf8",
  );
  assert.match(kt, new RegExp(`import ${EXPO_SERVICE.replace(/\./g, "\\.")}`));
});

test("ubrn regeneration keeps the hand-extended native files", () => {
  const cfg = readFileSync(join(MOBILE, "modules/pollis-native/ubrn.config.yaml"), "utf8");
  assert.match(cfg, /noOverwrite:\n\s+- android\/cpp-adapter\.cpp\n\s+- android\/build\.gradle/);
  const adapter = readFileSync(join(MOBILE, "modules/pollis-native/android/cpp-adapter.cpp"), "utf8");
  assert.match(adapter, /Java_com_pollisnative_PollisNotify_nativeFetch/);
  assert.match(adapter, /pollis_notify_fetch\(/);
});

test("the Android notification strings match the shared catalogues", async () => {
  const mod = await import(join(ROOT, "scripts/mobile-android-push-strings.mjs"));
  const expected = mod.expectedFiles() as Record<string, string>;
  assert.equal(Object.keys(expected).length, 7);
  for (const [rel, contents] of Object.entries(expected)) {
    const actual = readFileSync(join(mod.RES_DIR, rel), "utf8");
    assert.equal(actual, contents, `${rel} is stale: run node scripts/mobile-android-push-strings.mjs`);
    assert.match(contents, /pollis_push_generic_title/);
    assert.match(contents, /pollis_push_generic_body/);
  }
});

test("android string escaping", async () => {
  const mod = await import(join(ROOT, "scripts/mobile-android-push-strings.mjs"));
  assert.equal(mod.escapeAndroid("l'été & <b>"), "l\\'été &amp; &lt;b&gt;");
  assert.equal(mod.escapeAndroid("@me"), "\\@me");
});
