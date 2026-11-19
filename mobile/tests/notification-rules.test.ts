/* #2056: the phone reads a conversation only while the user is looking at it, and never presents
   a foreground push for the chat on screen (iOS now matches Android's native handler). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { readsOnArrival } from "../lib/unread.ts";
import { presentInForeground } from "../lib/push/presentation.ts";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (p: string) => readFileSync(join(root, p), "utf8");

const banner = { title: "New message", body: "You have a new message" };

test("a message arriving in the open chat reads it only in the foreground", () => {
  assert.equal(readsOnArrival("c1", "c1", "active"), true);
  assert.equal(readsOnArrival("c1", "c1", "background"), false, "a background ingest is not a read");
  assert.equal(readsOnArrival("c1", "c1", "inactive"), false);
  assert.equal(readsOnArrival("c2", "c1", "active"), false);
  assert.equal(readsOnArrival(null, "c1", "active"), false);
});

test("a foreground push for the chat on screen is not presented", () => {
  assert.equal(presentInForeground(banner, "c1", "c1", "active"), false);
});

test("every other foreground push still is", () => {
  assert.equal(presentInForeground(banner, "c2", "c1", "active"), true, "another conversation");
  assert.equal(presentInForeground(banner, null, "c1", "active"), true, "unresolved: show it");
  assert.equal(presentInForeground(banner, "c1", null, "active"), true, "no chat open");
  assert.equal(presentInForeground(banner, "c1", "c1", "background"), true, "app not on screen");
  assert.equal(presentInForeground({ title: "", body: "" }, "c2", null, "active"), false, "title-less");
});

test("the chat screen and the push handler use those rules", () => {
  const chat = read("app/chat/[id].tsx");
  assert.match(chat, /readsOnArrival\(appStore\.openConversationId, conversationId, AppState\.currentState\)/);
  assert.match(chat, /resumedToForeground\(previous, next\)/);
  const push = read("lib/push/index.ts");
  assert.match(push, /presentInForeground\(/);
});
