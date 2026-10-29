/* Self -> App -> Update (#1443): the channel each build path bakes in (app.config.js), and where each channel sends the user. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  parseInstallChannel,
  updateDestination,
  shortUpdateId,
  ANDROID_PACKAGE,
  SIDELOAD_PAGE_URL,
  WEBSITE_URL,
} from "../lib/installChannel.ts";

const require = createRequire(import.meta.url);
const here = dirname(fileURLToPath(import.meta.url));
const mobile = join(here, "..");
const repo = join(mobile, "..");
const appConfig = require("../app.config.js");
const fingerprint = require("../fingerprint.config.js");
const { installChannelFor, INSTALL_CHANNELS } = appConfig;

const CHANNEL_ENV = ["POLLIS_OTA", "POLLIS_RELEASE_KEY", "EXPO_PUBLIC_POLLIS_DELIVERY_URL", "EXPO_PUBLIC_R2_ENDPOINT", "EXPO_PUBLIC_R2_PUBLIC_URL", "EXPO_PUBLIC_POLLIS_DEV_ACCESS_KEY"];

function evaluate(env: Record<string, string | undefined>) {
  const saved = { ...process.env };
  try {
    for (const k of CHANNEL_ENV) {
      delete process.env[k];
    }
    Object.assign(process.env, env);
    const appJson = JSON.parse(readFileSync(join(mobile, "app.json"), "utf8"));
    return appConfig({ config: appJson.expo });
  } finally {
    process.env = saved;
  }
}

test("each build path bakes its own channel", () => {
  assert.equal(installChannelFor({ POLLIS_OTA: "production", POLLIS_RELEASE_KEY: "sideload" }), "sideload");
  assert.equal(installChannelFor({ POLLIS_OTA: "production" }), "store");
  assert.equal(installChannelFor({ POLLIS_OTA: "production", POLLIS_RELEASE_KEY: "upload" }), "store");
  assert.equal(installChannelFor({}), "dev");
  assert.equal(installChannelFor({ POLLIS_OTA: "off" }), "dev");
  assert.equal(installChannelFor({ POLLIS_RELEASE_KEY: "" }), "dev");
  assert.equal(installChannelFor({ POLLIS_RELEASE_KEY: "sideload" }), "sideload");
  assert.throws(() => installChannelFor({ POLLIS_RELEASE_KEY: "Sideload" }), /POLLIS_RELEASE_KEY must be/);
  for (const env of [{}, { POLLIS_OTA: "production" }, { POLLIS_RELEASE_KEY: "sideload" }]) {
    assert.ok(INSTALL_CHANNELS.includes(installChannelFor(env)));
    assert.equal(parseInstallChannel(installChannelFor(env)), installChannelFor(env));
  }
});

test("the channel is in extra, beside the EAS project id", () => {
  const dev = evaluate({});
  assert.equal(dev.extra.installChannel, "dev");
  assert.ok(dev.extra.eas?.projectId, "push needs extra.eas.projectId kept");
  assert.equal(evaluate({ POLLIS_RELEASE_KEY: "sideload" }).extra.installChannel, "sideload");
});

test("the fingerprint ignores the channel", () => {
  const hook = fingerprint.fileHookTransform;
  const hashed = (extra: Record<string, unknown>) =>
    hook({ type: "contents", id: "expoConfig" }, JSON.stringify({ name: "Pollis", extra }), true, "utf8");
  const base = { eas: { projectId: "p" } };
  for (const channel of ["store", "sideload", "dev"]) {
    assert.equal(hashed({ ...base, installChannel: channel }), JSON.stringify({ name: "Pollis", extra: base }));
  }
});

test("the sideload APK's native build step sets POLLIS_RELEASE_KEY=sideload", () => {
  const apk = readFileSync(join(repo, ".github", "workflows", "mobile-apk-release.yml"), "utf8");
  // The Gradle step's env decides the baked channel.
  const gradle = /- name: Assemble the signed release APKs\n([\s\S]*?)\n {6}- name:/.exec(apk)?.[1] ?? "";
  assert.match(gradle, /POLLIS_RELEASE_KEY: sideload/);
  assert.match(gradle, /POLLIS_OTA: production/);
  assert.match(apk, /installChannel/);
});

test("store installs open their store, the APK opens its page", () => {
  assert.deepEqual(updateDestination("store", "android"), {
    kind: "play",
    url: `market://details?id=${ANDROID_PACKAGE}`,
    fallbackUrl: `https://play.google.com/store/apps/details?id=${ANDROID_PACKAGE}`,
  });
  assert.equal(updateDestination("store", "android", { androidPackage: "x.y" }).url, "market://details?id=x.y");
  assert.deepEqual(updateDestination("store", "ios", { appStoreId: "123456789" }), {
    kind: "appstore",
    url: "itms-apps://apps.apple.com/app/id123456789",
    fallbackUrl: "https://apps.apple.com/app/id123456789",
  });
  assert.equal(updateDestination("store", "ios", { appStoreId: null }).kind, "testflight");
  assert.equal(updateDestination("store", "ios", { appStoreId: null }).url, "itms-beta://");
  assert.deepEqual(updateDestination("sideload", "android"), { kind: "web", url: SIDELOAD_PAGE_URL });
  assert.equal(SIDELOAD_PAGE_URL, "https://pollis.com/android");
  assert.deepEqual(updateDestination("dev", "android"), { kind: "web", url: WEBSITE_URL });
  assert.deepEqual(updateDestination("dev", "ios"), { kind: "web", url: WEBSITE_URL });
});

test("a binary from before the channel existed still gets a right answer", () => {
  assert.equal(parseInstallChannel(undefined), null);
  assert.equal(parseInstallChannel("play"), null);
  assert.deepEqual(updateDestination(null, "android"), { kind: "web", url: SIDELOAD_PAGE_URL });
  assert.equal(updateDestination(null, "ios", { appStoreId: null }).kind, "testflight");
  assert.equal(updateDestination(null, "ios", { appStoreId: "1" }).kind, "appstore");
});

test("the sideload page exists and links to Play", () => {
  const page = readFileSync(join(repo, "website", "android.html"), "utf8");
  assert.match(page, /play\.google\.com\/store\/apps\/details\?id=com\.pollis\.mobile/);
});

test("update ids shorten to eight hex digits", () => {
  assert.equal(shortUpdateId("0193A2B4-1C2D-7E8F-9A0B-1C2D3E4F5A6B"), "0193a2b4");
});
