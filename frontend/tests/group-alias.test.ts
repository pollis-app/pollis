/*
 * Per-group aliases (#2003): an alias overlays the main profile field by field,
 * a missing alias falls back, mentions keep their handle and render the alias,
 * and a save writes only what changed.
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  aliasAvatar,
  aliasName,
  fieldWrite,
  hasAlias,
  isValidAliasName,
  mentionLabels,
  toAliasMap,
  type GroupAlias,
} from "../src/utils/groupAlias.ts";

const alias = (user_id: string, display_name: string | null, avatar_url: string | null): GroupAlias => ({
  group_id: "g1",
  user_id,
  display_name,
  avatar_url,
});

const aliases = toAliasMap([
  alias("bob", "Bobby", null),
  alias("carol", null, "avatars/carol/c.jpg"),
]);

test("an alias name and avatar overlay the main profile field by field", () => {
  assert.equal(aliasName(aliases, "bob", "bob_handle"), "Bobby");
  assert.equal(aliasAvatar(aliases, "bob", "avatars/bob/main.jpg"), "avatars/bob/main.jpg", "no alias picture: main");
  assert.equal(aliasName(aliases, "carol", "carol"), "carol", "avatar-only alias keeps the name");
  assert.equal(aliasAvatar(aliases, "carol", "avatars/carol/main.jpg"), "avatars/carol/c.jpg");
});

test("no alias (no backfill, or outside a group) falls back to the main profile", () => {
  assert.equal(aliasName(aliases, "dave", "dave"), "dave");
  assert.equal(aliasName(undefined, "bob", "bob"), "bob", "a DM passes no aliases");
  assert.equal(aliasAvatar(toAliasMap([]), "bob", null), null);
  assert.equal(hasAlias(aliases, "bob"), true);
  assert.equal(hasAlias(aliases, "dave"), false);
});

test("mentions stay keyed by handle and render the alias", () => {
  const labels = mentionLabels(aliases, [
    { user_id: "bob", username: "Bob_Handle" },
    { user_id: "carol", username: "carol" },
    { user_id: "dave", username: "dave" },
  ]);
  assert.equal(labels.get("bob_handle"), "Bobby");
  assert.equal(labels.has("carol"), false, "an avatar-only alias leaves the mention as the handle");
  assert.equal(labels.has("dave"), false);
  assert.equal(mentionLabels(undefined, [{ user_id: "bob", username: "bob" }]).size, 0);
});

test("a save writes a field only when it changed; empty clears it", () => {
  assert.equal(fieldWrite("Bobby", "Bobby"), undefined);
  assert.equal(fieldWrite("Robert", "Bobby"), "Robert");
  assert.equal(fieldWrite(null, "Bobby"), "", "emptying the field clears it");
  assert.equal(fieldWrite(null, null), undefined);
  assert.equal(fieldWrite("New", undefined), "New");
});

test("alias names follow the DS's display-name rule", () => {
  assert.equal(isValidAliasName("Sam (Work)"), true);
  assert.equal(isValidAliasName("名前"), true);
  assert.equal(isValidAliasName("x".repeat(100)), true);
  assert.equal(isValidAliasName("x".repeat(101)), false);
  assert.equal(isValidAliasName("<b>"), false);
  assert.equal(isValidAliasName("a\nb"), false);
  assert.equal(isValidAliasName("‮evil"), false);
  assert.equal(isValidAliasName(""), false);
});
