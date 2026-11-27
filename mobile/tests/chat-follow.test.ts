/* Following new messages (#2125): the list follows only from the bottom, and counts what the reader missed. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  MAX_RISING,
  NEAR_BOTTOM_PT,
  classifyArrivals,
  distanceFromBottom,
  isNearBottom,
} from "../lib/chatFollow.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const chat = readFileSync(join(ROOT, "app/chat/[id].tsx"), "utf8");
const thread = readFileSync(join(ROOT, "app/chat/thread.tsx"), "utf8");

const mine = new Set(["me-1", "pending-1", "me-2"]);
const isOwn = (id: string) => mine.has(id);

test("at the bottom, a new message rises and the list follows", () => {
  const out = classifyArrivals({ prev: ["b", "a"], next: ["c", "b", "a"], atBottom: true, isOwn });
  assert.deepEqual(out, { rise: ["c"], unseen: 0, follow: true });
});

test("scrolled up, nothing moves and the message is counted", () => {
  const out = classifyArrivals({ prev: ["b", "a"], next: ["d", "c", "b", "a"], atBottom: false, isOwn });
  assert.deepEqual(out, { rise: [], unseen: 2, follow: false });
});

test("the first load and a change of conversation are not arrivals", () => {
  assert.deepEqual(classifyArrivals({ prev: null, next: ["b", "a"], atBottom: true, isOwn }).rise, []);
  assert.deepEqual(classifyArrivals({ prev: [], next: ["b", "a"], atBottom: true, isOwn }).rise, []);
  // Nothing in common: a different log, not new rows.
  const swapped = classifyArrivals({ prev: ["y", "x"], next: ["b", "a"], atBottom: true, isOwn });
  assert.deepEqual(swapped, { rise: [], unseen: 0, follow: false });
});

test("an older page lands at the oldest end and is never an arrival", () => {
  const out = classifyArrivals({ prev: ["c", "b"], next: ["c", "b", "a", "z"], atBottom: false, isOwn });
  assert.deepEqual(out, { rise: [], unseen: 0, follow: false });
});

test("a sent message's stub swapping for its stored row is not an arrival", () => {
  const out = classifyArrivals({ prev: ["pending-1", "b"], next: ["me-1", "b"], atBottom: true, isOwn });
  assert.deepEqual(out, { rise: [], unseen: 0, follow: false });
});

test("a swap and a real arrival together: only the arrival counts", () => {
  const out = classifyArrivals({
    prev: ["pending-1", "b"],
    next: ["c", "me-1", "b"],
    atBottom: false,
    isOwn,
  });
  assert.deepEqual(out, { rise: [], unseen: 1, follow: false });
});

test("the reader's own send always goes to the bottom, from anywhere, and counts nothing", () => {
  const out = classifyArrivals({ prev: ["b", "a"], next: ["pending-1", "b", "a"], atBottom: false, isOwn });
  assert.deepEqual(out, { rise: [], unseen: 0, follow: true });
});

test("a catch-up burst follows but does not rise", () => {
  const next = ["f", "e", "d", "c", "b"];
  assert.ok(next.length - 1 > MAX_RISING);
  const out = classifyArrivals({ prev: ["b"], next, atBottom: true, isOwn });
  assert.deepEqual(out, { rise: [], unseen: 0, follow: true });
});

test("near the bottom is measured from the newest message", () => {
  assert.equal(distanceFromBottom(0, true), 0);
  assert.equal(distanceFromBottom(120, true), 120);
  assert.equal(distanceFromBottom(-8, true), 0);
  assert.equal(distanceFromBottom(400, false, 1000, 600), 0);
  assert.equal(distanceFromBottom(100, false, 1000, 600), 300);
  assert.ok(isNearBottom(NEAR_BOTTOM_PT));
  assert.ok(!isNearBottom(NEAR_BOTTOM_PT + 1));
});

test("the chat no longer scrolls to the bottom on every new message", () => {
  assert.doesNotMatch(chat, /\[newestId\]\);/);
  assert.match(chat, /classifyArrivals\(/);
  // Scrolled up, the visible rows hold still while rows land under them.
  assert.match(chat, /maintainVisibleContentPosition=/);
  assert.match(chat, /<NewMessagesButton/);
});

test("the thread follows by the same rule", () => {
  assert.match(thread, /classifyArrivals\(/);
});
