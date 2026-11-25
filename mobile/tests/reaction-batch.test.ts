/* One reactions read per page, and only for the ids not yet loaded (#2115). Desktop: frontend/tests/reaction-prefetch.test.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import {
  mayReveal,
  mergeReactions,
  missingIds,
  readReactions,
  type ReactionInvoke,
  type ReactionMap,
} from "../hooks/queries/reactionBatch.ts";

const HOOK = fileURLToPath(new URL("../hooks/queries/useReactions.ts", import.meta.url));

const pill = (emoji: string, ...user_ids: string[]) => ({ emoji, user_ids, count: user_ids.length });

function fakeInvoke(server: Record<string, ReturnType<typeof pill>[]>) {
  const calls: Array<{ command: string; args: Record<string, unknown> }> = [];
  const invoke: ReactionInvoke = async <T,>(command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    assert.equal(command, "get_reactions_for_messages");
    const ids = args.messageIds as string[];
    return Object.fromEntries(ids.map((id) => [id, server[id] ?? []])) as T;
  };
  return { calls, invoke };
}

const page = (from: number, to: number) =>
  Array.from({ length: to - from }, (_, i) => `m${from + i}`);

test("a page of 50 is one invoke, and every id gets an entry", async () => {
  const fake = fakeInvoke({ m3: [pill("👍", "u1")] });
  const map = await readReactions(page(0, 50), fake.invoke);
  assert.equal(fake.calls.length, 1);
  assert.equal(map.size, 50);
  assert.deepEqual(map.get("m3"), [pill("👍", "u1")]);
  assert.deepEqual(map.get("m4"), [], "loaded, no reactions");
});

test("the next page reads only the ids the map lacks, and merges", async () => {
  const fake = fakeInvoke({ m3: [pill("👍", "u1")], m60: [pill("🎉", "u2")] });
  let map: ReactionMap = await readReactions(page(0, 50), fake.invoke);
  const loaded = [...page(50, 100), ...page(0, 50)];
  const missing = missingIds(map, loaded);
  assert.deepEqual(missing, page(50, 100));
  map = mergeReactions(map, await readReactions(missing, fake.invoke));
  assert.equal(fake.calls.length, 2);
  assert.deepEqual(fake.calls[1].args, { messageIds: page(50, 100) });
  assert.equal(map.size, 100);
  assert.deepEqual(map.get("m3"), [pill("👍", "u1")], "earlier entries kept");
  assert.deepEqual(map.get("m60"), [pill("🎉", "u2")]);
  assert.deepEqual(missingIds(map, loaded), [], "nothing left to read");
});

test("a new message is one read of that id alone", async () => {
  const fake = fakeInvoke({});
  const map = await readReactions(page(0, 50), fake.invoke);
  assert.deepEqual(missingIds(map, ["new", ...page(0, 50)]), ["new"]);
});

test("missingIds deduplicates and handles a cold cache", () => {
  assert.deepEqual(missingIds(undefined, ["a", "b", "a"]), ["a", "b"]);
});

test("no ids, no invoke", async () => {
  const fake = fakeInvoke({});
  const map = await readReactions([], fake.invoke);
  assert.equal(map.size, 0);
  assert.equal(fake.calls.length, 0);
});

test("pills come back sorted, as the row draws them", async () => {
  const fake = fakeInvoke({ m1: [pill("😀", "u1"), pill("👍", "u2")] });
  const map = await readReactions(["m1"], fake.invoke);
  assert.deepEqual(
    map.get("m1")!.map((r) => r.emoji),
    ["👍", "😀"].sort((a, b) => a.localeCompare(b)),
  );
});

test("a refresh replaces the entries it read and keeps the rest", () => {
  const prev: ReactionMap = new Map([
    ["a", [pill("👍", "u1")]],
    ["b", [pill("🎉", "u2")]],
  ]);
  const next = mergeReactions(prev, new Map([["a", []]]));
  assert.deepEqual(next.get("a"), []);
  assert.deepEqual(next.get("b"), [pill("🎉", "u2")]);
});

test("a fresh list waits for its reactions, or the timeout, then shows", () => {
  const ids = ["a", "b"];
  assert.equal(mayReveal(undefined, [], false), true, "nothing loaded: the loading or empty state shows");
  assert.equal(mayReveal(undefined, ids, false), false, "pills not known yet: hold");
  assert.equal(mayReveal(new Map([["a", []]]), ids, false), false, "one still missing: hold");
  assert.equal(mayReveal(new Map([["a", []], ["b", [pill("👍", "u1")]]]), ids, false), true);
  assert.equal(mayReveal(undefined, ids, true), true, "the wait ran out: show without them");
});

test("the chat screen hides its list only until then", () => {
  const screen = readFileSync(
    fileURLToPath(new URL("../app/chat/[id].tsx", import.meta.url)),
    "utf8",
  );
  assert.match(screen, /useRevealWithReactions\(/);
  assert.match(screen, /opacity: listRevealed \? 1 : 0/);
});

test("the hook makes no per-message reads", () => {
  const src = readFileSync(HOOK, "utf8");
  assert.doesNotMatch(src, /"get_reactions"/);
  assert.doesNotMatch(src, /Promise\.all/);
  assert.match(src, /readReactions\(missing, invoke\)/);
});
