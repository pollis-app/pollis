/*
 * Message-log sizing that has to track the user's font setting (#934).
 *
 * CLAUDE.md: sizes that scale with the font preference are in `rem`, never a
 * px constant. The row-height estimates already were; the load-more trigger
 * was a hardcoded `container.scrollTop < 150`. At the 15px default that is ten
 * lines of slack, but a reader on a 20px root gets taller rows and the same
 * 150px — so the trigger fires proportionally later in what they actually see.
 *
 * The scaling itself is pinned here as arithmetic, because that is what it is;
 * `loadMoreThresholdPx` only adds the `getComputedStyle` read, which needs a
 * browser. The browser half — that the log actually asks for another page —
 * is `e2e/message-window.spec.ts`.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { loadMoreThresholdPxAt } from "../src/components/Message/messageWindow.ts";

const MESSAGE_LIST = fileURLToPath(
  new URL("../src/components/Message/MessageList.tsx", import.meta.url),
);

test("the load-more threshold is unchanged at the default root font size", () => {
  // 15px is the app's `--font-size-base` default, and 150px is what the
  // constant used to be. This is the compatibility half: the fix must not
  // move the trigger for a user who never touched the font setting.
  assert.equal(loadMoreThresholdPxAt(15), 150);
});

test("the load-more threshold scales with the root font size", () => {
  assert.equal(loadMoreThresholdPxAt(30), 300, "double the root, double the slack");
  assert.equal(loadMoreThresholdPxAt(7.5), 75, "and halve it the other way");
  // The property that matters, stated directly: the threshold is a constant
  // number of LINES, whatever a line currently measures.
  for (const root of [10, 12, 15, 18, 20, 24]) {
    assert.equal(
      loadMoreThresholdPxAt(root) / root,
      loadMoreThresholdPxAt(15) / 15,
      `the threshold must be the same multiple of the root at ${root}px`,
    );
  }
});

test("GUARD: MessageList carries no px load-more constant (#934)", () => {
  const source = readFileSync(MESSAGE_LIST, "utf8");
  assert.doesNotMatch(
    source,
    /LOAD_MORE_THRESHOLD_PX/,
    "the px constant is back; the threshold belongs in messageWindow.ts, in rem",
  );
  assert.match(
    source,
    /loadMoreThresholdPx\(\)/,
    "MessageList must read the rem-derived threshold at call time, so it follows " +
      "a font-size change under a mounted log",
  );
});
