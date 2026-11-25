/* Reactions load with their page in one read and seed each row's cache (#2115). Mobile: mobile/tests/reaction-batch.test.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { QueryClient, QueryObserver } from "@tanstack/react-query";

import {
  prefetchReactions,
  reactableIds,
  reactionQueryKeys,
  reactionQueryOptions,
  settleWithin,
  type ReactionInvoke,
} from "../src/hooks/queries/reactionPrefetch.ts";

type Call = { command: string; args: Record<string, unknown> };

const THUMBS = [{ emoji: "👍", user_ids: ["u1"], count: 1 }];

function fakeInvoke(reactions: Record<string, typeof THUMBS> = { m2: THUMBS }) {
  const calls: Call[] = [];
  let release: () => void = () => undefined;
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let gated = false;
  const invoke: ReactionInvoke = async <T,>(command: string, args: Record<string, unknown>) => {
    calls.push({ command, args });
    if (gated) {
      await gate;
    }
    if (command === "get_reactions_for_messages") {
      const ids = args.messageIds as string[];
      return Object.fromEntries(ids.map((id) => [id, reactions[id] ?? []])) as T;
    }
    if (command === "get_reactions") {
      return (reactions[args.messageId as string] ?? []) as T;
    }
    throw new Error(`unexpected ${command}`);
  };
  return {
    calls,
    invoke,
    hold() {
      gated = true;
    },
    release: () => release(),
  };
}

function client() {
  return new QueryClient({ defaultOptions: { queries: { retry: false } } });
}

test("a page is ONE batched read that seeds every row, empty ones included", async () => {
  const qc = client();
  const fake = fakeInvoke();
  await prefetchReactions(qc, ["m1", "m2", "m3"], fake.invoke);
  assert.equal(fake.calls.length, 1);
  assert.equal(fake.calls[0].command, "get_reactions_for_messages");
  assert.deepEqual(fake.calls[0].args, { messageIds: ["m1", "m2", "m3"] });
  assert.deepEqual(qc.getQueryData(reactionQueryKeys.message("m1")), []);
  assert.deepEqual(qc.getQueryData(reactionQueryKeys.message("m2")), THUMBS);
  assert.deepEqual(qc.getQueryData(reactionQueryKeys.message("m3")), []);
});

test("a seeded row's useReactions does not read on its own", async () => {
  const qc = client();
  const fake = fakeInvoke();
  await prefetchReactions(qc, ["m1", "m2"], fake.invoke);
  const observer = new QueryObserver(qc, reactionQueryOptions("m2", fake.invoke));
  const unsubscribe = observer.subscribe(() => undefined);
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(fake.calls.length, 1, "no get_reactions after the batch");
  assert.deepEqual(observer.getCurrentResult().data, THUMBS);
  unsubscribe();
});

test("a row mounting while the batch is in flight waits on it instead of reading alone", async () => {
  const qc = client();
  const fake = fakeInvoke();
  fake.hold();
  const seeded = prefetchReactions(qc, ["m1", "m2"], fake.invoke);
  const observer = new QueryObserver(qc, reactionQueryOptions("m2", fake.invoke));
  const unsubscribe = observer.subscribe(() => undefined);
  fake.release();
  await seeded;
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual(
    fake.calls.map((c) => c.command),
    ["get_reactions_for_messages"],
  );
  assert.deepEqual(observer.getCurrentResult().data, THUMBS);
  unsubscribe();
});

test("only ids not already cached (or in flight) are read", async () => {
  const qc = client();
  const fake = fakeInvoke();
  await prefetchReactions(qc, ["m1", "m2"], fake.invoke);
  await prefetchReactions(qc, ["m1", "m2", "m3", "m3"], fake.invoke);
  assert.equal(fake.calls.length, 2);
  assert.deepEqual(fake.calls[1].args, { messageIds: ["m3"] });
  await prefetchReactions(qc, ["m1", "m2", "m3"], fake.invoke);
  assert.equal(fake.calls.length, 2, "a warm page reads nothing");
});

test("an invalidated row (a toggle settled) is re-read with the next batch", async () => {
  const qc = client();
  const fake = fakeInvoke();
  await prefetchReactions(qc, ["m1", "m2"], fake.invoke);
  await qc.invalidateQueries({ queryKey: reactionQueryKeys.message("m1") });
  await prefetchReactions(qc, ["m1", "m2"], fake.invoke);
  assert.deepEqual(fake.calls[1].args, { messageIds: ["m1"] });
});

test("a failed batch leaves the rows to read for themselves and never rejects", async () => {
  const qc = client();
  const invoke: ReactionInvoke = async () => {
    throw new Error("offline");
  };
  await prefetchReactions(qc, ["m1"], invoke);
  assert.equal(qc.getQueryData(reactionQueryKeys.message("m1")), undefined);
});

test("deleted messages draw no pills, so they are not asked about", () => {
  assert.deepEqual(
    reactableIds([{ id: "a" }, { id: "b", deleted_at: "2026-01-01" }, { id: "c", deleted_at: null }]),
    ["a", "c"],
  );
});

test("settleWithin resolves at the deadline when the work is slow", async () => {
  const started = Date.now();
  await settleWithin(new Promise(() => undefined), 20);
  assert.ok(Date.now() - started < 1000);
  await settleWithin(Promise.resolve(), 10_000);
});

test("the messages query seeds reactions and waits briefly on a first open", () => {
  const src = readFileSync(
    fileURLToPath(new URL("../src/hooks/queries/useMessages.ts", import.meta.url)),
    "utf8",
  );
  assert.match(src, /prefetchReactions\(queryClient, reactableIds\(messages\), invoke\)/);
  assert.match(src, /if \(cached === undefined\) \{\s*await settleWithin\(seeded, REACTIONS_REVEAL_WAIT_MS\);/);
});
