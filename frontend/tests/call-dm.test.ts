/* A 1:1 call shows its DM in the right panel (#2116): which DM, and only on the call route. */

import test from "node:test";
import assert from "node:assert/strict";

import { callIdFromPath, resolveCallDm } from "../src/utils/callDm.ts";
import type { DMConversation } from "../src/types/index.ts";

const ME = "u-alice";

function dm(id: string, peers: string[], updated = 0): DMConversation {
  return {
    id,
    user1_id: ME,
    user2_identifier: peers[0] ?? "",
    member_count: peers.length + 1,
    peers: peers.map((user_id) => ({ user_id })),
    created_at: 0,
    updated_at: updated,
  };
}

test("the DM whose members are exactly self and the peer is chosen", () => {
  const list = [dm("d-carol", ["u-carol"]), dm("d-bob", ["u-bob"])];
  assert.equal(resolveCallDm(list, "u-bob"), "d-bob");
  assert.equal(resolveCallDm(list, "u-carol"), "d-carol");
});

test("no 1:1 DM with the peer: no panel, and none is made up", () => {
  assert.equal(resolveCallDm([dm("d-carol", ["u-carol"])], "u-bob"), null);
  assert.equal(resolveCallDm([], "u-bob"), null);
});

test("a group DM that contains the peer is never chosen", () => {
  const group = dm("d-group", ["u-bob", "u-carol"]);
  assert.equal(resolveCallDm([group], "u-bob"), null);
  assert.equal(resolveCallDm([group, dm("d-bob", ["u-bob"])], "u-bob"), "d-bob");
});

test("a row whose count disagrees with one peer is not a 1:1", () => {
  const odd: DMConversation = { ...dm("d-odd", ["u-bob"]), member_count: 3 };
  assert.equal(resolveCallDm([odd], "u-bob"), null);
});

test("no peer known yet: no DM", () => {
  assert.equal(resolveCallDm([dm("d-bob", ["u-bob"])], null), null);
});

test("only the 1:1 call route carries a call id; group voice rooms never do", () => {
  assert.equal(callIdFromPath("/call/abc123"), "abc123");
  assert.equal(callIdFromPath("/call/abc%2D1"), "abc-1");
  for (const path of ["/", "/call", "/dms/d1", "/groups/g1/voice/v1", "/incoming-call/abc123"]) {
    assert.equal(callIdFromPath(path), null, path);
  }
});
