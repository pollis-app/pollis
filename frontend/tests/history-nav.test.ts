/*
 * Back/forward enablement — the pure arithmetic.
 *
 * The whole point of `utils/historyNav.ts` is that no browser API answers "can
 * I go forward?": `history.length` counts a session, not a direction, and it
 * does not shrink when a push throws the forward entries away. So the answer is
 * derived, and a derivation is exactly the kind of thing that goes subtly wrong
 * — a forward chevron that stays lit after a push points at entries that no
 * longer exist, and clicking it does nothing, which is the bug a user reports
 * as "the arrows are broken".
 *
 * These pin the contract the store and the chevrons both lean on: the ends
 * clamp, a push truncates the forward stack, a replace does not, and an entry
 * that arrives without an index still leaves the pair coherent.
 *
 *   node --test frontend/tests/
 */

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
  // The rule the whole file exists for. Two entries deep, one step back, then
  // a normal navigation: the entry that WAS ahead is gone, so forward has to
  // go dark — `history.length` would still say three and light it.
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
  // Replace swaps the entry under the cursor; nothing ahead of it is touched,
  // which is precisely how it differs from push.
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
  // A foreign pushState can leave `__TSR_index` off an entry. The pair must
  // still be coherent afterwards — above all it must never claim a negative
  // index or a forward entry the stack has never seen.
  const back = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "BACK" });
  assert.deepEqual(back, { index: 0, maxIndex: 0 });

  const forward = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "FORWARD" });
  assert.deepEqual(forward, { index: 0, maxIndex: 0 });

  const pushed = reduceHistoryNav(INITIAL_HISTORY_NAV, { action: "PUSH" });
  assert.deepEqual(pushed, { index: 1, maxIndex: 1 });
});

test("a nonsense index is refused rather than adopted", () => {
  // -1 and 1.5 are not stack positions. Adopting either would make `index >
  // maxIndex` reachable, and `canGoForward` is a comparison of exactly those.
  for (const index of [-1, 1.5, Number.NaN]) {
    const state = reduceHistoryNav({ index: 2, maxIndex: 3 }, {
      action: "GO",
      index,
    });
    assert.deepEqual(state, { index: 2, maxIndex: 3 });
  }
});

test("syncing an unwatched stack starts with forward dark", () => {
  // On attach we know where the cursor is and nothing about what is ahead of
  // it, so the ceiling starts AT the cursor: a lit forward chevron we cannot
  // justify is worse than a dark one that lights on the first real back step.
  assert.deepEqual(syncHistoryNav(4), { index: 4, maxIndex: 4 });
  assert.equal(canGoForward(syncHistoryNav(4)), false);
  assert.equal(canGoBack(syncHistoryNav(4)), true);

  // Nothing to read at all (a stack that predates the index stamp) is the
  // first entry, not a crash.
  assert.deepEqual(syncHistoryNav(undefined), INITIAL_HISTORY_NAV);
});
