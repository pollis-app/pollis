/* Channel categories (#2031): the sidebar's blocks, and the optimistic moves that mirror the DS. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  appendChannel,
  blockUnread,
  channelBlocks,
  collapsedVisible,
  deleteCategory,
  dropPosition,
  moveCategory,
  moveChannel,
} from "../src/utils/channelLayout.ts";

const ch = (id: string, category_id: string | null = null) => ({ id, category_id });
const cats = [
  { id: "k1", name: "Campaign" },
  { id: "k2", name: "Empty" },
];
// As the DS returns them: uncategorised first, then category by category.
const listed = [ch("a"), ch("b"), ch("c", "k1"), ch("d", "k1")];

const shape = (blocks: ReturnType<typeof channelBlocks>) =>
  blocks.map((b) => [b.category ? b.category.id : "-", b.channels.map((c) => c.id)]);

test("no categories: one headerless block, in the order given", () => {
  assert.deepEqual(shape(channelBlocks([ch("z"), ch("a")], [])), [["-", ["z", "a"]]]);
});

test("blocks keep list order, list empty categories, and file an unknown category under the top", () => {
  assert.deepEqual(shape(channelBlocks([...listed, ch("x", "gone")], cats)), [
    ["-", ["a", "b", "x"]],
    ["k1", ["c", "d"]],
    ["k2", []],
  ]);
});

test("moving a channel between blocks, within one, and past the end", () => {
  const into = moveChannel(listed, cats, "a", "k2", 0);
  assert.deepEqual(shape(channelBlocks(into, cats)), [["-", ["b"]], ["k1", ["c", "d"]], ["k2", ["a"]]]);
  assert.equal(into.find((c) => c.id === "a")?.category_id, "k2");

  const within = moveChannel(listed, cats, "d", "k1", 0);
  assert.deepEqual(shape(channelBlocks(within, cats))[1], ["k1", ["d", "c"]]);

  const out = moveChannel(listed, cats, "c", null, 99);
  assert.deepEqual(shape(channelBlocks(out, cats))[0], ["-", ["a", "b", "c"]]);
  assert.equal(out.find((c) => c.id === "c")?.category_id, null);
});

test("an unknown channel or category moves nothing", () => {
  assert.deepEqual(moveChannel(listed, cats, "nope", "k1", 0), listed);
  assert.deepEqual(moveChannel(listed, cats, "a", "nope", 0), listed);
});

test("deleting a category puts its channels at the end of the top block, keeping every one", () => {
  const next = deleteCategory([ch("c", "k1"), ch("a"), ch("d", "k1"), ch("b")], cats, "k1");
  assert.deepEqual(next.categories.map((k) => k.id), ["k2"]);
  assert.deepEqual(next.channels.map((c) => [c.id, c.category_id]), [
    ["a", null],
    ["b", null],
    ["c", null],
    ["d", null],
  ]);
});

test("categories reorder with clamping", () => {
  assert.deepEqual(moveCategory(cats, "k2", 0).map((k) => k.id), ["k2", "k1"]);
  assert.deepEqual(moveCategory(cats, "k1", 9).map((k) => k.id), ["k2", "k1"]);
});

test("a new channel lands at the bottom of its block", () => {
  assert.deepEqual(
    shape(channelBlocks(appendChannel(listed, cats, ch("n", "k1")), cats))[1],
    ["k1", ["c", "d", "n"]],
  );
  assert.deepEqual(shape(channelBlocks(appendChannel(listed, cats, ch("m")), cats))[0], ["-", ["a", "b", "m"]]);
});

test("a collapsed category sums its unread and keeps only the open channel and the joined voice room", () => {
  const unread: Record<string, number> = { c: 2, d: 3, a: 7 };
  assert.equal(blockUnread([ch("c"), ch("d")], (id) => unread[id] ?? 0), 5);
  assert.deepEqual(
    collapsedVisible([ch("c"), ch("d"), ch("v")], ["v", null, "c"]).map((c) => c.id),
    ["c", "v"],
  );
  assert.deepEqual(collapsedVisible([ch("c")], [null, undefined, ""]), []);
});

test("a drop slot becomes the DS position once the dragged row is lifted out", () => {
  const block = [ch("a"), ch("b"), ch("c")];
  // Dragging `a` to just above `c` (slot 2) leaves it at index 1.
  assert.equal(dropPosition(block, "a", 2), 1);
  // Dragging `c` to the top.
  assert.equal(dropPosition(block, "c", 0), 0);
  // From another block, slots are positions.
  assert.equal(dropPosition(block, "x", 2), 2);
});
