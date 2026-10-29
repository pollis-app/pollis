// Own-device messages show live but never count unread, and a missed wake-up is caught up.
import test from "node:test";
import assert from "node:assert/strict";

import {
  catchUpTargets,
  isOwnLatest,
  roomCatchUpTarget,
  shouldCountUnread,
} from "../lib/messageWake.ts";

const SELF = "01SELF";
const PEER = "01PEER";

test("our own message from another device is never unread", () => {
  assert.equal(
    shouldCountUnread({ latest: { sender_id: SELF }, selfId: SELF, isOpen: false }),
    false,
  );
});

test("someone else's message is unread unless the conversation is open", () => {
  assert.equal(
    shouldCountUnread({ latest: { sender_id: PEER }, selfId: SELF, isOpen: false }),
    true,
  );
  assert.equal(
    shouldCountUnread({ latest: { sender_id: PEER }, selfId: SELF, isOpen: true }),
    false,
  );
});

test("an unreadable result still counts rather than dropping someone else's message", () => {
  assert.equal(shouldCountUnread({ latest: null, selfId: SELF, isOpen: false }), true);
  assert.equal(isOwnLatest({ sender_id: SELF }, null), false);
});

const groups = [
  { id: "g1", channels: [{ id: "g1-general" }, { id: "g1-random" }] },
  { id: "g2", channels: [] },
];
const dms = [{ id: "dm1" }];

test("resume catches up one ingest per group and one per DM", () => {
  assert.deepEqual(catchUpTargets(groups, dms), [
    { id: "g1-general", kind: "channel" },
    { id: "dm1", kind: "dm" },
  ]);
});

test("a reconnected room maps to the ingest that covers it", () => {
  assert.deepEqual(roomCatchUpTarget("g1", groups, dms), { id: "g1-general", kind: "channel" });
  assert.deepEqual(roomCatchUpTarget("dm1", groups, dms), { id: "dm1", kind: "dm" });
  assert.equal(roomCatchUpTarget("inbox-01SELF", groups, dms), null);
  assert.equal(roomCatchUpTarget("g2", groups, dms), null);
  assert.equal(roomCatchUpTarget("unknown", groups, dms), null);
});
