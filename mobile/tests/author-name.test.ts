/*
 * Message author labels. Your own rows must read the same before and after a
 * send reconciles: the optimistic stub, send_message's row and the refetched
 * row carry different sender_username values, and the label used to flip from
 * "you" to the username mid-send.
 *
 *   cd mobile && pnpm test
 */

import test from "node:test";
import assert from "node:assert/strict";

import { authorName } from "../lib/authorName.ts";

const self = { id: "u1", username: "dan", preferred_name: "Dan K" };

test("own rows show the display name whatever sender_username says", () => {
  assert.equal(authorName("u1", undefined, self), "Dan K");
  assert.equal(authorName("u1", "dan", self), "Dan K");
  assert.equal(authorName("u1", "stale-name", self), "Dan K");
});

test("own rows fall back to the username without a display name", () => {
  const noDisplay = { id: "u1", username: "dan" };
  assert.equal(authorName("u1", undefined, noDisplay), "dan");
  assert.equal(authorName("u1", "dan", { ...noDisplay, preferred_name: "" }), "dan");
});

test("other people's rows show their sender_username", () => {
  assert.equal(authorName("u2", "alice", self), "alice");
});

test("an unknown author yields null for the caller's localized fallback", () => {
  assert.equal(authorName("u2", undefined, self), null);
  assert.equal(authorName("u2", undefined, null), null);
});
