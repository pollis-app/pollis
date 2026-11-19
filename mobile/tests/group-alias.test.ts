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
} from "../lib/groupAlias.ts";

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

// Wiring: the nudge, the bridge's argument names, and every group surface.
import { readFileSync } from "node:fs";
import { decodeRealtimeEvent, isTrustedOrigin } from "../lib/realtime/events.ts";

const read = (p: string) => readFileSync(new URL(p, import.meta.url), "utf8");

test("the DS's alias nudge decodes, and only from the DS", () => {
  const event = decodeRealtimeEvent(JSON.stringify({ type: "group_aliases_changed", group_id: "g1" }));
  assert.ok(event, "group_aliases_changed must decode");
  assert.equal(event.type, "group_aliases_changed");
  assert.ok(isTrustedOrigin(event, false));
  assert.ok(!isTrustedOrigin(event, true), "a participant cannot fake the DS nudge");
  assert.match(read("../hooks/useConversationRealtime.ts"), /group_aliases_changed[\s\S]*?aliasQueryKeys\.group\(event\.group_id\)/);
  assert.match(read("../hooks/useInboxRealtime.ts"), /case "group_aliases_changed":[\s\S]*?aliasQueryKeys\.all/);
});

test("the hooks speak the bridge's argument names", () => {
  const hooks = read("../hooks/queries/useGroupAliases.ts");
  const bridge = read("../../pollis-core/src/bridge.rs");
  assert.match(hooks, /"list_group_aliases", \{\s*groupId,\s*requesterId: currentUser\.id,/);
  assert.match(bridge, /"list_group_aliases" => \{[\s\S]*?"groupId"[\s\S]*?"requesterId"/);
  assert.match(hooks, /"set_group_alias", \{\s*userId: currentUser\.id,\s*groupId: vars\.groupId,\s*displayName: vars\.displayName,\s*avatarUrl: vars\.avatarUrl,/);
  assert.match(bridge, /"set_group_alias" => \{[\s\S]*?"userId"[\s\S]*?"groupId"[\s\S]*?"displayName"[\s\S]*?"avatarUrl"/);
  assert.match(hooks, /"reset_group_alias", \{\s*groupId: vars\.groupId,\s*userId: vars\.userId,\s*requesterId: currentUser\.id,/);
  assert.match(bridge, /"reset_group_alias" => \{[\s\S]*?"groupId"[\s\S]*?"userId"[\s\S]*?"requesterId"/);
  assert.match(hooks, /"upload_avatar_image", \{\s*userId,\s*path,/);
  assert.match(bridge, /"upload_avatar_image" => \{/);
});

test("every group surface resolves names and pictures through the group's aliases", () => {
  for (const rel of [
    "../app/chat/[id].tsx",
    "../app/chat/thread.tsx",
    "../app/conversation/pins.tsx",
    "../app/group/members.tsx",
    "../app/user/[id].tsx",
    "../hooks/useMentionCandidates.ts",
    "../components/chat/TypingIndicator.tsx",
  ]) {
    assert.match(read(rel), /useGroupAliases\(/, `${rel} reads the group's aliases`);
  }
  // Only in a channel: a DM shows main profiles.
  assert.match(read("../app/chat/[id].tsx"), /useGroupAliases\(kind === "channel" \? groupId \?\? null : null\)/);
  // Mentions keep the handle and render the alias.
  assert.match(read("../components/chat/MentionToken.tsx"), /@\{label \?\? name\}/);
});
