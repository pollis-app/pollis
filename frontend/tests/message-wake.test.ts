/*
 * A message sent from the user's OTHER device arrives on the same `new_message`
 * wake-up as anyone else's, and the wake-up names no sender. It must be shown,
 * but never alert or count as unread — decided after ingest from the newest
 * local row's MLS-attributed sender.
 *
 *   node --test frontend/tests/
 */
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
