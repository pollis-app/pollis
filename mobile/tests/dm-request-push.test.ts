// #1734: a phone is pushed for a DM request, and a tap opens the request list, never the chat.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { DM_REQUEST_PUSH_CAP, pushCapsFor, RICH_PUSH_CAP } from "../lib/push/caps.ts";
import {
  DM_REQUEST_KIND,
  DM_REQUESTS_PATH,
  requestsTapNavigation,
  tapTarget,
} from "../lib/push/tapNavigation.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(ROOT, p), "utf8");

test("the binary claims dm_request only from its embedded config", () => {
  assert.deepEqual(pushCapsFor({ dmRequestPush: true }, "ios"), [DM_REQUEST_PUSH_CAP]);
  assert.deepEqual(pushCapsFor({ dmRequestPush: true, richPush: { android: true } }, "android"), [
    RICH_PUSH_CAP,
    DM_REQUEST_PUSH_CAP,
  ]);
  // A binary from before #1734 embeds no flag and keeps today's pushes.
  assert.deepEqual(pushCapsFor({ richPush: { ios: true } }, "ios"), [RICH_PUSH_CAP]);
  assert.deepEqual(pushCapsFor({ dmRequestPush: "true" }, "ios"), []);
});

test("this build embeds the flag", () => {
  const extra = JSON.parse(read("app.json")).expo.extra;
  assert.equal(extra.dmRequestPush, true);
});

test("a request's handle routes to the request list, anything else to its conversation", () => {
  assert.deepEqual(tapTarget(DM_REQUEST_KIND, "01DM"), { to: "requests" });
  assert.deepEqual(tapTarget("dm", "01DM"), { to: "conversation", conversationId: "01DM" });
  assert.deepEqual(tapTarget("channel", "01CH"), { to: "conversation", conversationId: "01CH" });
});

test("the request list already on top is not stacked again; a chat is replaced", () => {
  assert.equal(requestsTapNavigation(DM_REQUESTS_PATH), "none");
  assert.equal(requestsTapNavigation("/chat/01OTHER"), "replace");
  assert.equal(requestsTapNavigation("/"), "push");
  assert.equal(requestsTapNavigation("/direct"), "push");
});

test("the push hook routes a request through tapTarget and refreshes the list on arrival", () => {
  const src = read("hooks/usePushNotifications.ts");
  assert.match(src, /tapTarget\(conv\.kind, conv\.conversationId\)/);
  assert.match(src, /DM_REQUESTS_PATH/);
  assert.match(src, /dmQueryKeys\.requests/);
});

test("accepting or blocking a request clears its notification on this phone at once", () => {
  assert.match(read("hooks/queries/useDMChannels.ts"), /dismissConversationNotifications\(dmChannelId\)/);
  assert.match(read("app/dm/requests.tsx"), /dismissConversationNotifications\(d\.id\)/);
});
