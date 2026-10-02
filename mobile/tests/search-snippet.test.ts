/*
 * Search-hit highlighting on mobile (#1202). The snippet is user content, so
 * it is sliced by index, never rendered as markup; these pin that the slicing
 * is lossless and robust to ranges the core should never send but might.
 */
import test from "node:test";
import assert from "node:assert/strict";

import { splitSnippet } from "../lib/searchSnippet.ts";

const join = (parts: { text: string }[]) => parts.map((p) => p.text).join("");

test("marks each highlighted range and keeps the text between", () => {
  assert.deepEqual(splitSnippet("the quick fox", [[4, 9]]), [
    { text: "the ", hit: false },
    { text: "quick", hit: true },
    { text: " fox", hit: false },
  ]);
});

test("out-of-order ranges are applied in order", () => {
  const parts = splitSnippet("a b c", [[4, 5], [0, 1]]);
  assert.deepEqual(parts.filter((p) => p.hit).map((p) => p.text), ["a", "c"]);
  assert.equal(join(parts), "a b c");
});

test("overlapping, empty, inverted and out-of-bounds ranges never corrupt the text", () => {
  const text = "hello world";
  for (const ranges of [
    [[0, 5], [3, 8]],
    [[2, 2]],
    [[6, 4]],
    [[-3, 2], [9, 50]],
  ] as [number, number][][]) {
    assert.equal(join(splitSnippet(text, ranges)), text, JSON.stringify(ranges));
  }
});

test("indices are UTF-16 code units, so astral characters stay whole", () => {
  // "🔒" is two UTF-16 units; the hit covers exactly "key".
  const text = "🔒 key";
  const parts = splitSnippet(text, [[3, 6]]);
  assert.deepEqual(parts, [
    { text: "🔒 ", hit: false },
    { text: "key", hit: true },
  ]);
});
