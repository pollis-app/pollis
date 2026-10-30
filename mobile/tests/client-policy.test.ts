/* The minimum-version check on mobile (#1611): the platform key, the 426 detection, the blocked
   screen's choices, the store and the dev override link. The rule itself is pollis-core's. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  applyCoreVerdict,
  bodyText,
  clientPlatformFor,
  getClientPolicy,
  isBlocked,
  isUpgradeRequiredError,
  launchWaitsForCheck,
  noticeUpgradeRequired,
  onUpgradeRequired,
  openLabelKey,
  parseDevPolicyLink,
  setClientPolicy,
  subscribeClientPolicy,
  versionsLine,
  type ClientPolicyStatus,
} from "../lib/clientPolicy.ts";
import { updateDestination } from "../lib/installChannel.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

function status(over: Partial<ClientPolicyStatus> = {}): ClientPolicyStatus {
  return {
    blocked: true,
    platform: "ios",
    version: "1.0.3",
    minimum: "1.0.21",
    message: null,
    source: "network",
    ...over,
  };
}

test("Play vs sideload comes from the baked install channel", () => {
  assert.equal(clientPlatformFor("ios", "store"), "ios");
  assert.equal(clientPlatformFor("ios", null), "ios");
  assert.equal(clientPlatformFor("android", "store"), "android-play");
  assert.equal(clientPlatformFor("android", "sideload"), "android-sideload");
  // A dev build is held to the sideload floor (it is not a Play install).
  assert.equal(clientPlatformFor("android", "dev"), "android-sideload");
  assert.equal(clientPlatformFor("android", null), "android-sideload");
});

test("the core's error is recognised however it was wrapped", () => {
  assert.ok(isUpgradeRequiredError(new Error("upgrade_required")));
  assert.ok(isUpgradeRequiredError("list_user_groups: upgrade_required"));
  assert.ok(!isUpgradeRequiredError(new Error("ds_post /v1/x 426 Upgrade Required")));
  assert.ok(!isUpgradeRequiredError(new Error("device_revoked")));
});

test("the open button names where this install updates from", () => {
  const label = (channel: "store" | "sideload" | "dev" | null, os: "ios" | "android", appStoreId: string | null = null) =>
    openLabelKey(updateDestination(channel, os, { appStoreId }).kind);
  assert.equal(label("store", "android"), "clientBlocked.openPlay");
  assert.equal(label("sideload", "android"), "clientBlocked.openWebsite");
  // iOS: TestFlight until the App Store record exists, then the App Store.
  assert.equal(label("store", "ios"), "clientBlocked.openTestflight");
  assert.equal(label("store", "ios", "1234567890"), "clientBlocked.openAppStore");
  assert.equal(label("dev", "ios"), "clientBlocked.openWebsite");
});

test("every key the screen uses exists in every locale", () => {
  const keys = [
    "clientBlocked.title",
    "clientBlocked.body",
    "clientBlocked.versions",
    "clientBlocked.installed",
    "clientBlocked.retry",
    "clientBlocked.checking",
    "clientBlocked.openFailed",
    ...(["appstore", "play", "testflight", "web"] as const).map(openLabelKey),
  ];
  for (const lng of ["en", "es", "fr", "ru", "uk", "zh", "ar"]) {
    const nav = JSON.parse(readFileSync(join(ROOT, `../frontend/src/i18n/locales/${lng}/nav.json`), "utf8"));
    for (const key of keys) {
      const value = key.split(".").reduce((o: Record<string, unknown> | undefined, k) => o?.[k] as Record<string, unknown> | undefined, nav);
      assert.equal(typeof value, "string", `${lng}: nav:${key}`);
    }
  }
});

test("the versions line and the manifest message", () => {
  assert.deepEqual(versionsLine(status()), {
    key: "clientBlocked.versions",
    vars: { version: "1.0.3", minimum: "1.0.21" },
  });
  assert.deepEqual(versionsLine(status({ minimum: null })), {
    key: "clientBlocked.installed",
    vars: { version: "1.0.3" },
  });
  assert.equal(versionsLine(status({ version: null })), null);
  assert.equal(bodyText(status(), "built-in"), "built-in");
  assert.equal(bodyText(status({ message: "  " }), "built-in"), "built-in");
  assert.equal(bodyText(status({ message: "Drafts are lost on 1.0.3." }), "built-in"), "Drafts are lost on 1.0.3.");
});

test("an upgrade_required asks the core once; only a blocked verdict blocks", async () => {
  setClientPolicy(status({ blocked: false, minimum: null, source: "network" }));
  let notified = 0;
  const unsubscribe = subscribeClientPolicy(() => {
    notified += 1;
  });
  // What the core answers to get_client_policy_status.
  let core: ClientPolicyStatus = status({ blocked: false, minimum: null, source: "network" });
  let asked = 0;
  const off = onUpgradeRequired(async () => {
    asked += 1;
    await settle();
    applyCoreVerdict(core);
  });

  noticeUpgradeRequired(new Error("network down"));
  await settle();
  await settle();
  assert.equal(asked, 0, "only upgrade_required asks");

  // A stray rejection while the core says allowed (e.g. one landing after an unblock): no block.
  noticeUpgradeRequired(new Error("upgrade_required"));
  assert.equal(isBlocked(), false, "the rejection alone never blocks");
  await settle();
  await settle();
  assert.equal(asked, 1);
  assert.equal(isBlocked(), false, "a not-blocked verdict leaves the store unblocked");

  // A real 426: the core says blocked.
  core = status({ blocked: true, source: "ds" });
  noticeUpgradeRequired(new Error("upgrade_required"));
  noticeUpgradeRequired("send_message: upgrade_required");
  await settle();
  await settle();
  assert.equal(asked, 2, "many failures ask once");
  assert.equal(isBlocked(), true);
  assert.equal(getClientPolicy()?.source, "ds");

  // Nothing else unblocks from here; a fresh check does.
  applyCoreVerdict(status({ blocked: false }));
  assert.equal(isBlocked(), true);
  applyCoreVerdict(null);
  assert.equal(isBlocked(), true);
  setClientPolicy(status({ blocked: false }));
  assert.equal(isBlocked(), false);
  assert.ok(notified >= 2);
  off();
  unsubscribe();
  setClientPolicy(null);
});

test("launch waits for the check unless the cache already allows this build", () => {
  // Allowed by the cache: go on, check in the background.
  assert.equal(launchWaitsForCheck(status({ blocked: false, source: "cache" })), false);
  // A cached block, no cache, or no answer at all: wait.
  assert.equal(launchWaitsForCheck(status({ blocked: true, source: "cache" })), true);
  assert.equal(launchWaitsForCheck(status({ blocked: false, source: "none" })), true);
  assert.equal(launchWaitsForCheck(null), true);
  assert.equal(launchWaitsForCheck(undefined), true);
});

test("dev override links", () => {
  const block = encodeURIComponent('{"v":1,"minimum":{"ios":"999.0.0"}}');
  assert.deepEqual(parseDevPolicyLink(`pollis://dev/client-policy?policy=${block}`), {
    kind: "set",
    policy: '{"v":1,"minimum":{"ios":"999.0.0"}}',
  });
  assert.deepEqual(parseDevPolicyLink("pollis:///dev/client-policy?url=http%3A%2F%2F127.0.0.1%3A9%2Fp.json"), {
    kind: "set",
    url: "http://127.0.0.1:9/p.json",
  });
  assert.deepEqual(parseDevPolicyLink("pollis://dev/client-policy?clear=1"), { kind: "clear" });
  assert.equal(parseDevPolicyLink("pollis://dev/client-policy"), null);
  assert.equal(parseDevPolicyLink("pollis://self/preferences"), null);
  assert.equal(parseDevPolicyLink("https://evil.example/dev/client-policy?clear=1"), null);
  assert.equal(parseDevPolicyLink("pollis://dev/client-policy?policy=%E0%A4%A"), null);
});

test("the root layout gates on the check before anything routes, and only dev builds take overrides", () => {
  const layout = readFileSync(join(ROOT, "app/_layout.tsx"), "utf8");
  assert.match(layout, /check_client_policy/);
  assert.match(layout, /peek_client_policy/, "launch reads the cache first");
  assert.match(layout, /launchWaitsForCheck\(cached\)/);
  assert.match(layout, /applyCoreVerdict\(/, "a 426 blocks only on the core's verdict");
  assert.match(layout, /!policyReady\) \{\s*return null;/, "the splash is held until the check lands");
  assert.match(layout, /blocked && policy \? \(\s*<ClientBlockedScreen/, "the blocked screen replaces the navigator");
  assert.match(layout, /__DEV__ \|\| getBuildInfo\(\)\.channel === "dev"/);
  assert.match(layout, /clientVersion: build\.version/, "the native version, never the OTA bundle's");
  const bridge = readFileSync(join(ROOT, "lib/native/bridge.ts"), "utf8");
  assert.match(bridge, /noticeUpgradeRequired\(failure\)/);
});
