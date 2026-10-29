/* Back/forward enablement arithmetic: ends clamp, push truncates forward, replace doesn't, unstamped entries stay coherent. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  canGoBack,
  canGoForward,
  reduceHistoryNav,
  syncHistoryNav,
  INITIAL_HISTORY_NAV,
  type HistoryNavEvent,
  type HistoryNavState,
} from "../src/utils/historyNav.ts";

const walk = (
  state: HistoryNavState,
  events: HistoryNavEvent[],
): HistoryNavState => events.reduce(reduceHistoryNav, state);

test("a fresh stack can go neither way", () => {
  assert.equal(canGoBack(INITIAL_HISTORY_NAV), false);
  assert.equal(canGoForward(INITIAL_HISTORY_NAV), false);
});

test("pushing opens back and leaves forward dark", () => {
  const state = reduceHistoryNav(INITIAL_HISTORY_NAV, {
    action: "PUSH",
    index: 1,
  });
  assert.deepEqual(state, { index: 1, maxIndex: 1 });
  assert.equal(canGoBack(state), true);
  assert.equal(canGoForward(state), false);
});

test("stepping back opens forward without losing the ceiling", () => {
  const state = walk(INITIAL_HISTORY_NAV, [
    { action: "PUSH", index: 1 },
    { action: "PUSH", index: 2 },
    { action: "BACK", index: 1 },
  ]);
  assert.deepEqual(state, { index: 1, maxIndex: 2 });
  assert.equal(canGoBack(state), true);
  assert.equal(canGoForward(state), true);
});

test("returning to the far end closes forward again", () => {
  const state = walk(INITIAL_HISTORY_NAV, [
    { action: "PUSH", index: 1 },
    { action: "BACK", index: 0 },
    { action: "FORWARD", index: 1 },
  ]);
  assert.deepEqual(state, { index: 1, maxIndex: 1 });
  assert.equal(canGoForward(state), false);
});

test("a push from the middle discards the entries ahead of it", () => {
  // A push from the middle drops the forward entry.
  const state = walk(INITIAL_HISTORY_NAV, [
    { action: "PUSH", index: 1 },
    { action: "PUSH", index: 2 },
    { action: "BACK", index: 1 },
    { action: "PUSH", index: 2 },
  ]);
  assert.deepEqual(state, { index: 2, maxIndex: 2 });
  assert.equal(canGoForward(state), false);
});

test("a replace keeps the forward stack it navigated within", () => {
  const state = walk(INITIAL_HISTORY_NAV, [
    { action: "PUSH", index: 1 },
    { action: "PUSH", index: 2 },
    { action: "BACK", index: 1 },
    { action: "REPLACE", index: 1 },
  ]);
  assert.deepEqual(state, { index: 1, maxIndex: 2 });
  assert.equal(canGoForward(state), true);
});

test("a multi-entry GO lands where the entry says it landed", () => {
  const state = walk(INITIAL_HISTORY_NAV, [
    { action: "PUSH", index: 1 },
    { action: "PUSH", index: 2 },
    { action: "PUSH", index: 3 },
    { action: "GO", index: 1 },
  ]);
  assert.deepEqual(state, { index: 1, maxIndex: 3 });
  assert.equal(canGoBack(state), true);
  assert.equal(canGoForward(state), true);
});

test("an unstamped entry is inferred from the action, and stays clamped", () => {
  // A foreign pushState without `__TSR_index` must stay coherent.
  const back = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "BACK" });
  assert.deepEqual(back, { index: 0, maxIndex: 0 });

  const forward = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "FORWARD" });
  assert.deepEqual(forward, { index: 0, maxIndex: 0 });

  const pushed = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "PUSH" });
  assert.deepEqual(pushed, { index: 1, maxIndex: 1 });
});

test("a nonsense index is refused rather than adopted", () => {
  // Non-positions would make `index > maxIndex` reachable.
  for (const index of [-1, 1.5, Number.NaN]) {
    const state = reduceHistoryNav({ index: 2, maxIndex: 3 }, {
      action: "GO",
      index,
    });
    assert.deepEqual(state, { index: 2, maxIndex: 3 });
  }
});

test("syncing an unwatched stack starts with forward dark", () => {
  // On attach the ceiling starts at the cursor.
  assert.deepEqual(syncHistoryNav(4), { index: 4, maxIndex: 4 });
  assert.equal(canGoForward(syncHistoryNav(4)), false);
  assert.equal(canGoBack(syncHistoryNav(4)), true);

  assert.deepEqual(syncHistoryNav(undefined), INITIAL_HISTORY_NAV);
});
