/* #1614: a tap on a notification Android's rich-push handler posted (rich, name-only, generic, the
   deadline fallback) cold-starts the app into that conversation, as FCM's own notification did. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { takeLaunchTap, type LaunchResponse } from "../lib/push/launchTap.ts";
import { createTapGate } from "../lib/push/tapGate.ts";
import { readPayloadRouting } from "../lib/push/routing.ts";

const MOBILE = join(dirname(fileURLToPath(import.meta.url)), "..");
const H = "frmyJ8y2K7GmeaWCPBjWRg";

/**
 * What expo-notifications makes of the extras PushTapActivity hands it: Android's
 * `NotificationSerializer.toResponseBundleFromExtras` (a JSON `body` is the data, else the extras
 * are) and then JS's `mapNotificationContent` (parses `dataString`).
 */
function expoResponseFromExtras(extras: Record<string, string>): LaunchResponse {
  const body = extras.body;
  let data: unknown = extras;
  if (body !== undefined) {
    try {
      data = JSON.parse(body);
    } catch {
      data = extras;
    }
  }
  return { notification: { request: { content: { data } } } };
}

/** expo's last-response store, holding the launch tap the way NotificationsEmitter does. */
function store(initial: LaunchResponse | null) {
  let last = initial;
  return {
    getLast: () => last,
    clear: () => {
      last = null;
    },
  };
}

/** The cold start as `usePushNotifications` runs it: take the launch tap, hold it until past the PIN. */
function coldStart(response: LaunchResponse | null) {
  const routed: string[] = [];
  const gate = createTapGate<unknown>((data) => {
    const r = readPayloadRouting(data);
    if (r) {
      routed.push(r.handle);
    }
  });
  const s = store(response);
  const launched = takeLaunchTap(s);
  if (launched !== null) {
    gate.tap(launched);
  }
  return { routed, gate, store: s };
}

// The extras RichPush.tapExtras builds (RichPushTest.kt pins that side) for each delivery shape.
const SHAPES: Record<string, Record<string, string>> = {
  "Expo push service envelope": {
    body: JSON.stringify({ h: H }),
    scopeKey: "@pollis/mobile",
    experienceId: "@pollis/mobile",
    "google.message_id": "0:1%abc",
  },
  "raw FCM data": { h: H, "google.message_id": "0:1%abc" },
  "debug hook (no FCM message id)": { body: JSON.stringify({ h: H }), "google.message_id": H },
};

for (const [shape, extras] of Object.entries(SHAPES)) {
  test(`cold start routes a tap by its handle: ${shape}`, () => {
    const { routed, gate, store: s } = coldStart(expoResponseFromExtras(extras));
    // Held while the PIN screen is up.
    assert.deepEqual(routed, []);
    gate.setReady(true);
    assert.deepEqual(routed, [H]);
    // Taken once: a later remount of the hook does not route it again.
    assert.equal(s.getLast(), null);
    assert.equal(takeLaunchTap(s), null);
  });
}

test("a cold start from the launcher routes nothing", () => {
  const { routed, gate } = coldStart(null);
  gate.setReady(true);
  assert.deepEqual(routed, []);
});

test("a store that throws degrades to opening the app", () => {
  assert.equal(
    takeLaunchTap({
      getLast: () => {
        throw new Error("unavailable");
      },
      clear: () => {},
    }),
    null,
  );
});

test("the hook takes the launch tap through takeLaunchTap", () => {
  const push = readFileSync(join(MOBILE, "lib/push/index.ts"), "utf8");
  assert.match(push, /takeLaunchTap\(\{/);
  const hook = readFileSync(join(MOBILE, "hooks/usePushNotifications.ts"), "utf8");
  assert.match(hook, /takeLaunchNotificationData\(\)/);
});

test("every natively posted notification taps through PushTapActivity", () => {
  const dir = join(MOBILE, "modules/pollis-native/android/src/main");
  const notifier = readFileSync(join(dir, "java/com/pollisnative/push/RichPushNotifier.kt"), "utf8");
  // One content intent, set on the builder every plan starts from.
  assert.equal(notifier.match(/setContentIntent\(/g)?.length, 1);
  assert.match(notifier, /\.setContentIntent\(tapIntent\(handle, data, messageId, sentTime\)\)/);
  assert.match(notifier, /Intent\(context, PushTapActivity::class\.java\)/);
  assert.match(notifier, /RichPush\.tapExtras\(handle, data, messageId\)/);
  // Not the launcher intent: Android hands that to a re-created MainActivity's onNewIntent, which
  // expo drops before the app has loaded (#1614).
  assert.doesNotMatch(notifier, /getLaunchIntentForPackage/);
  const activity = readFileSync(join(dir, "java/com/pollisnative/push/PushTapActivity.kt"), "utf8");
  assert.match(activity, /NotificationManager\.onNotificationResponseFromExtras\(/);
  const manifest = readFileSync(join(dir, "AndroidManifestNew.xml"), "utf8");
  assert.match(manifest, /android:name="com\.pollisnative\.push\.PushTapActivity"/);
  assert.match(manifest, /android:exported="false"/);
});
