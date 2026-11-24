/* Your own rows' author label never flips mid-send. */

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

test("#2085: other people's rows show their display name first, the handle without one", () => {
  assert.equal(authorName("u2", "nicholas.dinh31_wnkd", self, "Nick"), "Nick");
  assert.equal(authorName("u2", "nick", self, null), "nick");
  assert.equal(authorName("u2", "nick", self, ""), "nick");
  assert.equal(authorName("u2", undefined, self, "Nick"), "Nick");
});

test("#2085: own rows fall back to the row's display name before the handle", () => {
  const noDisplay = { id: "u1", username: "dan" };
  assert.equal(authorName("u1", "dan", noDisplay, "Dan from row"), "Dan from row");
  assert.equal(authorName("u1", "dan", self, "Dan from row"), "Dan K");
});

test("an unknown author yields null for the caller's localized fallback", () => {
  assert.equal(authorName("u2", undefined, self), null);
  assert.equal(authorName("u2", undefined, null), null);
});
