/* The load-more threshold scales with the font setting (#934); the browser half is `e2e/message-window.spec.ts`. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { loadMoreThresholdPxAt } from "../src/components/Message/messageWindow.ts";

const MESSAGE_LIST = fileURLToPath(
  new URL("../src/components/Message/MessageList.tsx", import.meta.url),
);

test("the load-more threshold is unchanged at the default root font size", () => {
  // The 15px default keeps the old 150px.
  assert.equal(loadMoreThresholdPxAt(15), 150);
});

test("the load-more threshold scales with the root font size", () => {
  assert.equal(loadMoreThresholdPxAt(30), 300, "double the root, double the slack");
  assert.equal(loadMoreThresholdPxAt(7.5), 75, "and halve it the other way");
  // A constant number of lines at any root size.
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
