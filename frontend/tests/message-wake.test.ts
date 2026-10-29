/* Our own other-device messages show but never alert or count unread, decided after ingest. */
import test from "node:test";
import assert from "node:assert/strict";

import { isOwnLatest, shouldAlertForWake } from "../src/utils/messageWake.ts";

const SELF = "01SELF";
const PEER = "01PEER";

test("our own message from another device never alerts", () => {
  assert.equal(
    shouldAlertForWake({ latest: { sender_id: SELF }, selfId: SELF, isSelected: false }),
    false,
  );
});

test("someone else's message alerts when the conversation is not on screen", () => {
  assert.equal(
    shouldAlertForWake({ latest: { sender_id: PEER }, selfId: SELF, isSelected: false }),
    true,
  );
});

test("nothing alerts for the conversation on screen", () => {
  assert.equal(
    shouldAlertForWake({ latest: { sender_id: PEER }, selfId: SELF, isSelected: true }),
    false,
  );
});

test("an unreadable result still alerts rather than dropping someone else's message", () => {
  assert.equal(shouldAlertForWake({ latest: null, selfId: SELF, isSelected: false }), true);
  assert.equal(isOwnLatest(undefined, SELF), false);
});

test("own-ness keys on the sender's user id only when we know who we are", () => {
  assert.equal(isOwnLatest({ sender_id: SELF }, null), false);
  assert.equal(isOwnLatest({ sender_id: SELF }, SELF), true);
});
