/*
 * Clearing read notifications from the tray (#1508): `mobile/lib/push/dismiss.ts`
 * and the wiring around it.
 *
 * A message notification used to stay in the tray after its message was read,
 * unless it was the one the user tapped. Every tray notification is now matched
 * by its opaque handle — `data.h` when the app presented it, the Android tag
 * (which the DS sets to the handle) when the system posted it — and cleared on
 * this device's own read, on the DS's silent `{ d: [...] }` push after a read
 * anywhere, and on a foreground catch-up for a push that never arrived.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  handleOfPresented,
  MAX_HANDLES,
  pickDismissals,
  readDismissPayload,
  trayHandles,
} from "../lib/push/dismiss.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

const H1 = "aGFuZGxlLW9uZQ";
const H2 = "aGFuZGxlLXR3bw";

test("a notification the app presented is matched by its payload handle", () => {
  assert.equal(handleOfPresented({ identifier: "0:1730000000000%abc", data: { h: H1 } }), H1);
});

test("a notification Android posted in the background is matched by its tag", () => {
  // What expo-notifications reports for a notification it did not build: the
  // payload is gone, the tag (= the handle) survives in the identifier.
  const identifier = `expo-notifications://foreign_notifications?tag=${H2}&id=0`;
  assert.equal(handleOfPresented({ identifier, data: { "android.title": "New message" } }), H2);
  // Order of the query parameters does not matter.
  assert.equal(handleOfPresented({ identifier: `expo-notifications://foreign_notifications?id=0&tag=${H2}`, data: null }), H2);
});

test("anything else has no handle", () => {
  for (const n of [
    { identifier: "expo-notifications://foreign_notifications?id=7", data: {} },
    // FCM's own default tag, from a push sent before the DS set one.
    { identifier: "expo-notifications://foreign_notifications?tag=FCM-Notification%3A123&id=0", data: {} },
    { identifier: "some-local-notification", data: { conversationId: "c1" } },
    { identifier: "x", data: { h: "" } },
    { identifier: "x", data: { h: 42 } },
    { identifier: "x", data: { h: "<script>" } },
  ]) {
    assert.equal(handleOfPresented(n), null, JSON.stringify(n));
  }
});

test("the silent push is read in the shapes the listener and the task see", () => {
  // As sent.
  assert.deepEqual(readDismissPayload({ d: [H1, H2] }), [H1, H2]);
  // As the background task receives it on both platforms: the data blob as a
  // JSON string under `dataString`.
  assert.deepEqual(
    readDismissPayload({ data: { dataString: JSON.stringify({ d: [H1] }) }, notification: null }),
    [H1],
  );
  // Android also spreads the raw FCM data, whose values are strings.
  assert.deepEqual(readDismissPayload({ data: { d: JSON.stringify([H2]) } }), [H2]);
});

test("an ordinary message push, or anything malformed, dismisses nothing", () => {
  for (const p of [
    null,
    "d",
    { h: H1 },
    { data: { dataString: JSON.stringify({ h: H1 }) } },
    { data: { dataString: "{not json" } },
    { d: "not json" },
    { d: [] },
    { d: [42, null, ""] },
    // A tap response on Android also reaches the task.
    { actionIdentifier: "expo.modules.notifications.actions.DEFAULT", notification: {} },
  ]) {
    assert.equal(readDismissPayload(p), null, JSON.stringify(p));
  }
});

test("a silent push names at most one query's worth of handles", () => {
  const many = Array.from({ length: MAX_HANDLES + 20 }, (_, i) => `h${i}`);
  assert.equal(readDismissPayload({ d: many })?.length, MAX_HANDLES);
});

test("only the named notifications are cleared, whichever way they were posted", () => {
  const tray = [
    { identifier: "presented-1", data: { h: H1 } },
    { identifier: `expo-notifications://foreign_notifications?tag=${H2}&id=0`, data: {} },
    { identifier: "unrelated", data: { h: "b3RoZXI" } },
    { identifier: "no-handle", data: {} },
  ];
  assert.deepEqual(pickDismissals(tray, [H1, H2]), [
    "presented-1",
    `expo-notifications://foreign_notifications?tag=${H2}&id=0`,
  ]);
  assert.deepEqual(pickDismissals(tray, []), []);
  assert.deepEqual(trayHandles(tray), [H1, H2, "b3RoZXI"]);
});

test("the background task is registered before the app renders", () => {
  const pkg = JSON.parse(read("package.json"));
  assert.equal(pkg.main, "index.ts");
  assert.ok(pkg.dependencies["expo-task-manager"], "expo-task-manager is a direct dependency");
  const entry = read("index.ts");
  const task = entry.indexOf('import "./lib/push/dismissTask";');
  const router = entry.indexOf('import "expo-router/entry";');
  assert.ok(task >= 0 && router > task, "the task is defined before expo-router loads the app");
  const src = read("lib/push/dismissTask.ts");
  assert.match(src, /TaskManager\.defineTask/);
  assert.match(src, /Notifications\.registerTaskAsync\(DISMISS_TASK\)/);
  // Headless-safe: no native bridge, no store.
  assert.doesNotMatch(src, /from "\.\.\/native"|appStore/);
});

test("iOS may wake the app for the silent push", () => {
  const app = JSON.parse(read("app.json")).expo;
  const plugin = app.plugins.find((p: unknown) => Array.isArray(p) && p[0] === "expo-notifications");
  assert.equal(plugin?.[1]?.enableBackgroundRemoteNotifications, true);
});

test("every read clears this device's tray, and landing/resume catches up", () => {
  const hook = read("hooks/queries/useUnread.ts");
  assert.match(
    hook,
    /appStore\.markRead\(conversationId\);\s*void dismissConversationNotifications\(conversationId\);/,
  );
  // Both the landing effect and the foreground effect reconcile.
  assert.equal(hook.match(/void reconcileReadNotifications\(userId\);/g)?.length, 2);
  const push = read("lib/push/index.ts");
  assert.match(push, /invoke<string\[\]>\("dismissed_push_handles", \{ userId, handles \}\)/);
  // The catch-up trusts only the DS's record of a read, never local counts: a
  // notification for a message not yet ingested here would look read.
  assert.doesNotMatch(push.slice(push.indexOf("export async function reconcileReadNotifications")), /get_unread_counts|unreadCounts/);
});
