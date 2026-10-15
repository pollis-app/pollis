/*
 * Runs of lost-message placeholders collapse into one row (#1312).
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import { collapseUndecryptable } from "../src/utils/collapseUndecryptable.ts";

const msg = (id: string) => ({ id });
const lost = (id: string) => ({ id, undecryptable: true });

test("a run becomes one row that keeps its first id and counts the rest", () => {
  const out = collapseUndecryptable([msg("a"), lost("b"), lost("c"), lost("d"), msg("e")]);
  assert.deepEqual(
    out.map((m) => [m.id, (m as { undecryptable_count?: number }).undecryptable_count]),
    [["a", undefined], ["b", 3], ["e", undefined]],
  );
});

test("a real message between two placeholders keeps them apart", () => {
  const out = collapseUndecryptable([lost("a"), msg("b"), lost("c")]);
  assert.deepEqual(
    out.map((m) => [m.id, (m as { undecryptable_count?: number }).undecryptable_count]),
    [["a", 1], ["b", undefined], ["c", 1]],
  );
});

test("nothing lost, nothing changed", () => {
  const input = [msg("a"), msg("b")];
  assert.deepEqual(collapseUndecryptable(input), input);
});
