/* Channel categories (#2031): mobile's layout module is desktop's file, byte for byte; the panel
   speaks the bridge's argument names; Edit mode is admin-only and its moves step across labels. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { appendChannel, channelBlocks, dropPosition, flattenBlocks, stepCategory, stepChannel } from "../lib/channelLayout.ts";

const here = dirname(fileURLToPath(import.meta.url));
const read = (p: string) => readFileSync(join(here, p), "utf8");

test("mobile/lib/channelLayout.ts is desktop's channelLayout.ts, byte for byte", () => {
  assert.equal(
    read("../lib/channelLayout.ts"),
    read("../../frontend/src/utils/channelLayout.ts"),
    "edit frontend/src/utils/channelLayout.ts and copy it to mobile/lib/channelLayout.ts",
  );
});

const ch = (id: string, category_id: string | null = null) => ({ id, category_id });
const cats = [
  { id: "k1", name: "One" },
  { id: "k2", name: "Two" },
];
const blocks = channelBlocks([ch("a"), ch("b"), ch("c", "k1"), ch("d", "k2")], cats);

test("Move up / Move down step a channel one slot, across a label at a block's edge", () => {
  assert.deepEqual(stepChannel(blocks, "a", 1), { categoryId: null, position: 1 });
  assert.deepEqual(stepChannel(blocks, "b", 1), { categoryId: "k1", position: 0 });
  assert.deepEqual(stepChannel(blocks, "c", -1), { categoryId: null, position: 2 });
  assert.deepEqual(stepChannel(blocks, "c", 1), { categoryId: "k2", position: 0 });
  assert.equal(stepChannel(blocks, "a", -1), null);
  assert.equal(stepChannel(blocks, "d", 1), null);
});

test("a category steps among categories and stops at the ends", () => {
  assert.equal(stepCategory(cats, "k1", 1), 1);
  assert.equal(stepCategory(cats, "k1", -1), null);
  assert.equal(stepCategory(cats, "k2", 1), null);
});

test("the hooks speak the bridge's argument names", () => {
  const hooks = read("../hooks/queries/useChannelCategories.ts");
  const bridge = read("../../pollis-core/src/bridge.rs");
  for (const [cmd, args] of [
    ["create_channel_category", ["groupId", "name", "requesterId"]],
    ["rename_channel_category", ["groupId", "categoryId", "name", "requesterId"]],
    ["delete_channel_category", ["groupId", "categoryId", "requesterId"]],
    ["move_channel_category", ["groupId", "categoryId", "position", "requesterId"]],
    ["move_channel", ["groupId", "channelId", "categoryId", "position", "requesterId"]],
  ] as const) {
    assert.ok(hooks.includes(`"${cmd}"`), `${cmd} is called`);
    const arm = bridge.slice(bridge.indexOf(`"${cmd}" => {`));
    const body = arm.slice(0, arm.indexOf("ok("));
    for (const a of args) {
      assert.ok(body.includes(`"${a}"`), `${cmd} reads ${a}`);
    }
  }
  assert.match(read("../hooks/queries/useUserGroups.ts"), /categoryId: vars\.categoryId \?\? null/);
});

test("Edit mode and every arranging control are for admins only", () => {
  const panel = read("../components/groups/GroupPanel.tsx");
  assert.match(panel, /const arranging = editing && isAdmin;/);
  assert.match(panel, /\{isAdmin \? \(\s*<>\s*<Button\s+testID=\{arranging \? "btn-edit-channels-done" : "btn-edit-channels"\}/);
  assert.match(panel, /editing=\{arranging\}/);
});

test("collapse state stays on the device", () => {
  const store = read("../lib/categoryCollapse.ts");
  assert.match(store, /documentDirectory/);
  assert.doesNotMatch(store, /invoke\(|save_preferences/);
});

test("a new channel lands at the bottom of its block, and a drop slot counts the lifted row out", () => {
  const next = appendChannel([ch("a"), ch("c", "k1")], cats, ch("n", "k1"));
  assert.deepEqual(next.map((c) => c.id), ["a", "c", "n"]);
  assert.deepEqual(
    flattenBlocks(channelBlocks(next, cats)).map((c) => [c.id, c.category_id]),
    [["a", null], ["c", "k1"], ["n", "k1"]],
  );
  assert.equal(dropPosition([ch("a"), ch("b"), ch("c")], "a", 2), 1);
});
