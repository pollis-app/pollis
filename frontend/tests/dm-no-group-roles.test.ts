/* A DM has no admins (#2126): the open chat's group is a channel's, never the group left selected behind a DM. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { chatGroupId } from "../src/utils/chatGroup.ts";

test("a channel's chat belongs to its group", () => {
  assert.equal(chatGroupId({ selectedChannelId: "c1", selectedGroupId: "g1" }), "g1");
});

test("a DM opened while a group is still selected has no group, so no roles", () => {
  assert.equal(chatGroupId({ selectedChannelId: null, selectedGroupId: "g1" }), null);
});

test("nothing open: no group", () => {
  assert.equal(chatGroupId({ selectedChannelId: null, selectedGroupId: null }), null);
});

const SRC = (p: string) => readFileSync(fileURLToPath(new URL(`../src/${p}`, import.meta.url)), "utf8");

test("the chat view resolves admins, aliases and moderation from the chat's group only", () => {
  for (const file of [
    "components/Layout/MainContent.tsx",
    "hooks/queries/useMentionCandidates.ts",
    "hooks/queries/useCommandCandidates.ts",
  ]) {
    assert.doesNotMatch(SRC(file), /selectedGroupId/, `${file} reads selectedGroupId; use appStore.chatGroupId`);
  }
});
