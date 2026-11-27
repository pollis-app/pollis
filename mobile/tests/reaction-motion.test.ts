/* Reaction pills move only for a change the reader can see happen (#2125). */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  LOCAL_ADD_WINDOW_MS,
  noteLocalReaction,
  pillMotion,
  wasJustAdded,
} from "../lib/reactionMotion.ts";
import { MOTION } from "../../design/theme.ts";

const never = () => false;
const pills = (...xs: [string, number][]) => xs.map(([emoji, count]) => ({ emoji, count }));

test("a row's first paint moves nothing", () => {
  const out = pillMotion(null, pills(["👍", 2], ["🎉", 1]), never);
  assert.deepEqual([...out.pop], []);
  assert.deepEqual([...out.roll], []);
});

test("a pill the reader just added pops even on a row that had none", () => {
  const out = pillMotion(null, pills(["👍", 1]), (e) => e === "👍");
  assert.deepEqual([...out.pop], ["👍"]);
});

test("after that, a new pill pops and a changed count rolls", () => {
  const prev = new Map([["👍", 1], ["🎉", 3]]);
  const out = pillMotion(prev, pills(["👍", 2], ["🎉", 3], ["🔥", 1]), never);
  assert.deepEqual([...out.pop], ["🔥"]);
  assert.deepEqual([...out.roll], ["👍"]);
});

test("a local add is live only briefly", () => {
  noteLocalReaction("m1", "👍", 1000);
  assert.ok(wasJustAdded("m1", "👍", 1000 + LOCAL_ADD_WINDOW_MS));
  assert.ok(!wasJustAdded("m1", "👍", 1001 + LOCAL_ADD_WINDOW_MS));
  assert.ok(!wasJustAdded("m2", "👍", 1000));
});

test("the pop and the bump stay inside the shared limits", () => {
  assert.ok(MOTION.popPeakScale - 1 <= MOTION.maxOvershoot + 1e-9);
  assert.ok(MOTION.bumpScale - 1 <= MOTION.maxOvershoot + 1e-9);
  const pillsSrc = readFileSync(
    fileURLToPath(new URL("../components/chat/ReactionPills.tsx", import.meta.url)),
    "utf8",
  );
  // The bump is skipped under Reduce Motion; the pop and the roll become a fade.
  assert.match(pillsSrc, /if \(!reduceMotion\) \{\s*scale\.value = withSequence/);
  assert.match(pillsSrc, /pop \? \(reduceMotion \? REDUCED_FADE_IN : POP_IN\)/);
  assert.match(pillsSrc, /roll \? \(reduceMotion \? REDUCED_FADE_IN : ROLL_IN\)/);
});

test("every add marks the pill to pop, from wherever it came", () => {
  const hook = readFileSync(
    fileURLToPath(new URL("../hooks/queries/useReactions.ts", import.meta.url)),
    "utf8",
  );
  assert.match(hook, /if \(vars\.mode === "add"\) \{\s*noteLocalReaction\(vars\.messageId, vars\.emoji\);/);
});
