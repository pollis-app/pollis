import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { carryStubs, isLocalStub, settleSend } from "../hooks/queries/pendingMessages.ts";

const row = (id: string, extra: { pending?: boolean; failed?: boolean } = {}) => ({ id, ...extra });

test("only local optimistic rows count as stubs", () => {
  assert.equal(isLocalStub(row("pending-1", { pending: true })), true);
  assert.equal(isLocalStub(row("pending-1", { failed: true })), true);
  assert.equal(isLocalStub(row("pending-1")), false);
  assert.equal(isLocalStub(row("01ABC", { pending: true })), false);
});

test("a refetch mid-send keeps the stub at the new end", () => {
  const cached = [row("pending-9", { pending: true }), row("b"), row("a")];
  // The store does not have the sending row yet, so the refetch lacks it.
  assert.deepEqual(carryStubs([row("b"), row("a")], cached).map((m) => m.id), ["pending-9", "b", "a"]);
  // A thread is oldest-first: the stub stays last.
  assert.deepEqual(
    carryStubs([row("a"), row("b")], [row("a"), row("b"), row("pending-9", { pending: true })], "oldestFirst").map((m) => m.id),
    ["a", "b", "pending-9"],
  );
  // A failed stub is kept too, so a failed send stays labelled, never vanishes.
  assert.deepEqual(carryStubs([row("a")], [row("pending-3", { failed: true })]).map((m) => m.id), ["pending-3", "a"]);
  // Nothing in flight: the fetched rows as they are.
  const fetched = [row("a")];
  assert.equal(carryStubs(fetched, [row("a")]), fetched);
  assert.equal(carryStubs(fetched, undefined), fetched);
});

test("a settling send swaps the stub, or restores the row if a refetch lost it", () => {
  const confirmed = row("01NEW");
  assert.deepEqual(settleSend([row("pending-9", { pending: true }), row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
  // The stub was dropped by a refetch that ran before this fix's carry-over.
  assert.deepEqual(settleSend([row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
  assert.deepEqual(settleSend([row("a")], "pending-9", confirmed, "oldestFirst").map((m) => m.id), ["a", "01NEW"]);
  // A refetch already brought the confirmed row in: no duplicate.
  assert.deepEqual(settleSend([row("01NEW"), row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
});

test("both message queries carry stubs and both settle paths use settleSend", () => {
  const s = readFileSync(new URL("../hooks/queries/useMessages.ts", import.meta.url), "utf8");
  assert.equal(s.match(/carryStubs\(/g)?.length, 2);
  assert.equal(s.match(/settleSend\(/g)?.length, 2);
  // A read that started before the row was stored is cancelled and redone.
  assert.match(s, /settleSend\(p\.messages, ctx\.optimisticId, settled\)[\s\S]{0,400}invalidateQueries\(\{ queryKey: ctx\.key \}\)/);
});
