/* The right panel's context comes from the route, nothing else. */

import test from "node:test";
import assert from "node:assert/strict";

import { panelContextFromPath } from "../src/components/Layout/RightPanel/panelContext.ts";

const none = { groupId: null, channelId: null, conversationId: null };

test("every page under a group is that group; a channel also carries its id", () => {
  assert.deepEqual(panelContextFromPath("/groups/g1"), { groupId: "g1", channelId: null, conversationId: null });
  assert.deepEqual(panelContextFromPath("/groups/g1/channels/c1"), { groupId: "g1", channelId: "c1", conversationId: null });
  assert.deepEqual(panelContextFromPath("/groups/g1/channels/c1/rename"), { groupId: "g1", channelId: "c1", conversationId: null });
  for (const sub of ["members", "members/u2/kick", "invite", "leave", "emoji", "rename", "join-requests", "channels/new", "voice/v1"]) {
    assert.deepEqual(panelContextFromPath(`/groups/g1/${sub}`), { groupId: "g1", channelId: null, conversationId: null }, sub);
  }
});

test("a DM and its settings are that conversation", () => {
  assert.deepEqual(panelContextFromPath("/dms/d1"), { groupId: null, channelId: null, conversationId: "d1" });
  assert.deepEqual(panelContextFromPath("/dms/d1/settings"), { groupId: null, channelId: null, conversationId: "d1" });
});

test("pages about no conversation have no context, static siblings included", () => {
  for (const path of ["/", "/groups", "/groups/new", "/groups/search", "/dms", "/dms/new", "/dms/requests", "/dms/blocked", "/settings", "/preferences", "/arcade", "/report/u2/d1/m1", "/vault"]) {
    assert.deepEqual(panelContextFromPath(path), none, path);
  }
});
