// Runs of lost-message placeholders collapse into one row (#1312).
import test from "node:test";
import assert from "node:assert/strict";

import { collapseUndecryptable } from "../lib/collapseUndecryptable.ts";

type M = { id: string; undecryptable?: boolean; undecryptableCount?: number };
const msg = (id: string): M => ({ id });
const lost = (id: string): M => ({ id, undecryptable: true });
const shape = (ms: M[]) =>
  ms.map((m) => [m.id, m.undecryptableCount]);

test("a run becomes one row that keeps its first id and counts the rest", () => {
  assert.deepEqual(
    shape(collapseUndecryptable([msg("a"), lost("b"), lost("c"), lost("d"), msg("e")])),
    [["a", undefined], ["b", 3], ["e", undefined]],
  );
});

test("a real message between two placeholders keeps them apart", () => {
  assert.deepEqual(
    shape(collapseUndecryptable([lost("a"), msg("b"), lost("c")])),
    [["a", 1], ["b", undefined], ["c", 1]],
  );
});
