/* A notification tap never stacks a duplicate of the chat already on screen. */

import test from "node:test";
import assert from "node:assert/strict";

import { tapNavigation } from "../lib/push/tapNavigation.ts";

test("a tap for the chat already on top navigates nowhere", () => {
  assert.equal(tapNavigation("/chat/01ABC", "01ABC"), "none");
});

test("a tap from a different chat replaces it, so back goes to the list", () => {
  assert.equal(tapNavigation("/chat/01OTHER", "01ABC"), "replace");
  assert.equal(tapNavigation("/chat/thread", "01ABC"), "replace");
});

test("a tap from anywhere else pushes the chat", () => {
  assert.equal(tapNavigation("/", "01ABC"), "push");
  assert.equal(tapNavigation("/groups", "01ABC"), "push");
  assert.equal(tapNavigation("/group/01G", "01ABC"), "push");
});

test("an id that merely prefixes the open chat's is a different chat", () => {
  assert.equal(tapNavigation("/chat/01ABCD", "01ABC"), "replace");
});
