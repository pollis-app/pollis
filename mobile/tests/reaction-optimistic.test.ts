/*
 * The reaction map a tap produces, before the write has answered (#1028).
 *
 * `useToggleReaction` awaited the invoke and then invalidated, so a pill did
 * not move until a round trip and the refetch behind it had both landed.
 * `applyReactionToggle` computes the post-write map up front — and it has to
 * agree with what `useConversationReactions` will report, or every tap ends in
 * a visible correction when the refetch arrives.
 *
 * That agreement is the thing worth testing: pill ordering, a message dropping
 * out of the map, `count` tracking `user_ids`, and a toggle that asks for what
 * is already true.
 *
 *   node --test mobile/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { applyReactionToggle } from "../hooks/queries/reactionOptimistic.ts";
import type { Reaction } from "../hooks/queries/useReactions.ts";

const ME = "u-me";
const THEM = "u-them";
const MSG = "m1";

const map = (entries: Array<[string, Reaction[]]> = []) =>
  new Map<string, Reaction[]>(entries);

const pill = (emoji: string, ...users: string[]): Reaction => ({
  emoji,
  user_ids: users,
  count: users.length,
});

test("reacting to a message with no reactions creates the pill", () => {
  const next = applyReactionToggle(map(), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "add",
  });
  assert.deepEqual(next.get(MSG), [pill("👍", ME)]);
});

test("joining an existing pill appends rather than replacing", () => {
  const next = applyReactionToggle(map([[MSG, [pill("👍", THEM)]]]), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "add",
  });
  assert.deepEqual(next.get(MSG), [pill("👍", THEM, ME)]);
});

// The queryFn sorts what the Rust HashMap hands back, so an optimistic insert
// that appended would reorder on the refetch — a visible twitch after the tap.
test("a new pill lands in the sorted position the refetch will put it in", () => {
  const next = applyReactionToggle(map([[MSG, [pill("🅰", THEM), pill("🅾", THEM)]]]), {
    messageId: MSG,
    emoji: "🅱",
    userId: ME,
    mode: "add",
  });
  assert.deepEqual(
    next.get(MSG)?.map((r) => r.emoji),
    ["🅰", "🅱", "🅾"],
  );
});

test("leaving a pill others are still in keeps it, minus you", () => {
  const next = applyReactionToggle(map([[MSG, [pill("👍", THEM, ME)]]]), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "remove",
  });
  assert.deepEqual(next.get(MSG), [pill("👍", THEM)]);
});

test("leaving the last pill drops it", () => {
  const next = applyReactionToggle(map([[MSG, [pill("👍", ME), pill("🎉", THEM)]]]), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "remove",
  });
  assert.deepEqual(next.get(MSG), [pill("🎉", THEM)]);
});

// `useConversationReactions` omits messages with no reactions rather than
// storing an empty array, so an empty array here would render an empty pill
// row until the refetch removed it.
test("removing the only reaction takes the message out of the map", () => {
  const next = applyReactionToggle(map([[MSG, [pill("👍", ME)]]]), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "remove",
  });
  assert.equal(next.has(MSG), false);
});

test("count is derived from user_ids and cannot drift from it", () => {
  const next = applyReactionToggle(map([[MSG, [pill("👍", THEM)]]]), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "add",
  });
  const got = next.get(MSG)![0];
  assert.equal(got.count, got.user_ids.length);
  assert.equal(got.count, 2);
});

test("a double tap cannot double-count", () => {
  const once = applyReactionToggle(map(), {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "add",
  });
  const twice = applyReactionToggle(once, {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "add",
  });
  assert.deepEqual(twice.get(MSG), [pill("👍", ME)]);
  // Same map by identity, so React Query re-renders nothing.
  assert.equal(twice, once);
});

test("removing a reaction you never left is a no-op", () => {
  const before = map([[MSG, [pill("👍", THEM)]]]);
  const after = applyReactionToggle(before, {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "remove",
  });
  assert.equal(after, before);
});

test("other messages are untouched", () => {
  const before = map([
    [MSG, [pill("👍", ME)]],
    ["m2", [pill("🎉", THEM)]],
  ]);
  const after = applyReactionToggle(before, {
    messageId: MSG,
    emoji: "👍",
    userId: ME,
    mode: "remove",
  });
  assert.deepEqual(after.get("m2"), [pill("🎉", THEM)]);
  // A new Map, so the cache sees a changed reference.
  assert.notEqual(after, before);
});
