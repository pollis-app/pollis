/* Live arrivals in the log (#2125): slide in only at the bottom, count when scrolled up, and never
   for the initial load, an older page or a stub's swap for its confirmed row. The browser half is
   `e2e/message-motion.spec.ts`. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { appendedKeys, classifyAppend, isAtBottom } from "../src/components/Message/liveAppend.ts";

const nobody = () => false;
const ctx = (over: Partial<Parameters<typeof classifyAppend>[2]> = {}) => ({
  atBottom: true,
  liveTail: true,
  isOwn: nobody,
  ...over,
});
const snap = (keys: string[], logKey = "c1") => ({ logKey, keys });

test("a message appended at the bottom slides in", () => {
  const r = classifyAppend(snap(["a", "b"]), snap(["a", "b", "c"]), ctx());
  assert.deepEqual(r, { animate: ["c"], unseen: 0, jumpToEnd: false });
});

test("scrolled up: nothing moves, the arrival is counted", () => {
  const r = classifyAppend(snap(["a", "b"]), snap(["a", "b", "c", "d"]), ctx({ atBottom: false }));
  assert.deepEqual(r, { animate: [], unseen: 2, jumpToEnd: false });
});

test("scrolled up and the reader sends: they are taken to it, nothing counted", () => {
  const r = classifyAppend(
    snap(["a"]),
    snap(["a", "pending-1"]),
    ctx({ atBottom: false, isOwn: (k) => k === "pending-1" }),
  );
  assert.deepEqual(r, { animate: [], unseen: 0, jumpToEnd: true });
});

test("the initial load is not an arrival", () => {
  assert.deepEqual(classifyAppend(null, snap(["a", "b"]), ctx()).animate, []);
  assert.deepEqual(classifyAppend(snap([]), snap(["a", "b"]), ctx()).animate, []);
});

test("switching conversations is not an arrival", () => {
  const r = classifyAppend(snap(["a"], "c1"), snap(["a", "x"], "c2"), ctx());
  assert.deepEqual(r.animate, []);
  assert.equal(r.unseen, 0);
});

test("an older page prepended is not an arrival", () => {
  const r = classifyAppend(snap(["c", "d"]), snap(["a", "b", "c", "d"]), ctx({ atBottom: false }));
  assert.deepEqual(r, { animate: [], unseen: 0, jumpToEnd: false });
});

test("a stub swapped for its confirmed row does not arrive twice", () => {
  assert.deepEqual(appendedKeys(["a", "pending-1"], ["a", "m9"]), []);
  // A real arrival in the same update still counts.
  assert.deepEqual(appendedKeys(["a", "pending-1"], ["a", "m9", "m10"]), ["m10"]);
});

test("an anchored window on older history never animates or counts (#1039)", () => {
  const r = classifyAppend(snap(["a"]), snap(["a", "b"]), ctx({ liveTail: false, atBottom: false }));
  assert.deepEqual(r, { animate: [], unseen: 0, jumpToEnd: false });
});

test("a replaced log with nothing in common is not an append", () => {
  assert.deepEqual(appendedKeys(["a", "b"], ["x", "y"]), []);
});

test("at the bottom means within the threshold of the end", () => {
  assert.equal(isAtBottom({ scrollHeight: 1000, scrollTop: 600, clientHeight: 380 }, 24), true);
  assert.equal(isAtBottom({ scrollHeight: 1000, scrollTop: 500, clientHeight: 380 }, 24), false);
});

test("MessageList classifies during render and follows without smooth scroll under reduced motion", () => {
  const src = readFileSync(
    fileURLToPath(new URL("../src/components/Message/MessageList.tsx", import.meta.url)),
    "utf8",
  );
  assert.match(src, /classifyAppend\(/);
  assert.match(src, /reduceMotion \? "auto" : "smooth"/);
  assert.match(src, /<NewMessagesButton/);
});
