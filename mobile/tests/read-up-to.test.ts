// The read-up-to avatar (#2140): level with the newest message of mine the other person has read,
// placed by reading alone (never delivery), and one element that glides rather than per-row marks.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { readUpToId } from "../lib/readUpTo.ts";

const me = "u-alice";
const bob = "u-bob";
const own = (id: string) => ({ id, sender_id: me });
const peer = (id: string) => ({ id, sender_id: bob });
const receipts = (rows: Record<string, string[]>) =>
  new Map(Object.entries(rows).map(([id, read_by]) => [id, { read_by }]));

test("sits on the newest own message the peer has read", () => {
  const newestFirst = [own("m4"), peer("p3"), own("m2"), own("m1")];
  assert.equal(readUpToId(newestFirst, me, bob, receipts({ m1: [bob], m2: [bob], m4: [] })), "m2");
  assert.equal(readUpToId(newestFirst, me, bob, receipts({ m1: [bob], m2: [bob], m4: [bob] })), "m4");
});

test("nothing shows until something is read: delivery never places it", () => {
  assert.equal(readUpToId([own("m1")], me, bob, receipts({ m1: [] })), null);
  assert.equal(readUpToId([own("m1")], me, bob, new Map()), null);
  assert.equal(readUpToId([own("m1")], me, bob, undefined), null);
});

test("never on the peer's own messages, nor without a viewer or a peer", () => {
  assert.equal(readUpToId([peer("p1")], me, bob, receipts({ p1: [bob] })), null);
  assert.equal(readUpToId([own("m1")], null, bob, receipts({ m1: [bob] })), null);
  assert.equal(readUpToId([own("m1")], me, null, receipts({ m1: [bob] })), null);
});

test("someone else's read does not count", () => {
  assert.equal(readUpToId([own("m1")], me, bob, receipts({ m1: ["u-carol"] })), null);
});

test("the rail glides on the shared timing and honours Reduce Motion", () => {
  const src = readFileSync(new URL("../components/chat/ReadRail.tsx", import.meta.url), "utf8");
  assert.match(src, /withTiming\(0, BASE_TIMING\)/, "the glide uses the shared base timing");
  assert.match(src, /useReduceMotion\(\)/, "Reduce Motion is read from the app's one source");
  assert.match(src, /reduceMotion\)\s*\{\s*opacity\.value = 1;\s*return;/, "under Reduce Motion it moves without the glide");
});

test("the chat screen draws the rail only for a one-to-one DM with receipts on", () => {
  const src = readFileSync(new URL("../app/chat/[id].tsx", import.meta.url), "utf8");
  assert.match(src, /isDm && onePeer && sendReadReceipts \? onePeer : null/);
  // The rows are memoised MessageRowItems (#2141): the rule sits in the row context.
  assert.match(src, /showReceipts: isDm && dmPeers\.length > 1/, "a larger DM keeps the count tick");
  const item = readFileSync(new URL("../components/chat/MessageRowItem.tsx", import.meta.url), "utf8");
  assert.match(item, /showReceipt=\{mine && ctx\.showReceipts\}/);
  assert.match(item, /<ReadRailSlot messageId=\{m\.id\} peer=\{readUpTo \? railPeer : null\} \/>/);
});
