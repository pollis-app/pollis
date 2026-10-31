/*
 * DM receipts (#1637): one tick per conversation, on the viewer's newest own
 * message that has a receipt to show, never one on every message.
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { receiptTargetId } from "../src/utils/receiptTarget.ts";

const me = "u-alice";
const own = (id: string) => ({ id, sender_id: me });
const peer = (id: string) => ({ id, sender_id: "u-bob" });
const delivered = (id: string) => ({ message_id: id, delivered_by: ["u-bob"], read_by: [] as string[] });
const read = (id: string) => ({ message_id: id, delivered_by: ["u-bob"], read_by: ["u-bob"] });
const map = (...rows: { message_id: string; delivered_by: string[] }[]) =>
  new Map(rows.map((r) => [r.message_id, r]));

test("only the newest own message carries the tick, even when older ones were read", () => {
  const newestFirst = [own("m3"), own("m2"), own("m1")];
  assert.equal(receiptTargetId(newestFirst, me, map(read("m1"), read("m2"), delivered("m3"))), "m3");
});

test("the peer's messages never carry it, and a reply after mine does not move it", () => {
  const newestFirst = [peer("p2"), own("m2"), peer("p1"), own("m1")];
  assert.equal(receiptTargetId(newestFirst, me, map(read("m1"), read("m2"))), "m2");
});

test("a newer own message nobody has acknowledged yet leaves the tick where it was", () => {
  const newestFirst = [own("pending-1"), own("m2"), own("m1")];
  assert.equal(receiptTargetId(newestFirst, me, map(read("m1"), read("m2"))), "m2");
  // A row with an empty delivered set draws nothing, so it cannot hold the tick either.
  const empty = new Map([["m3", { delivered_by: [] as string[] }], ["m2", read("m2")]]);
  assert.equal(receiptTargetId([own("m3"), own("m2")], me, empty), "m2");
});

test("nothing to show: no receipts, no viewer, or no own message", () => {
  assert.equal(receiptTargetId([own("m1")], me, undefined), null);
  assert.equal(receiptTargetId([own("m1")], me, new Map()), null);
  assert.equal(receiptTargetId([own("m1")], undefined, map(read("m1"))), null);
  assert.equal(receiptTargetId([peer("p1")], me, map(read("p1"))), null);
});

test("mobile carries the same rule", () => {
  const desktop = readFileSync(new URL("../src/utils/receiptTarget.ts", import.meta.url), "utf8");
  const mobile = readFileSync(new URL("../../mobile/lib/receiptTarget.ts", import.meta.url), "utf8");
  const body = (src: string) => src.slice(src.indexOf("export interface ReceiptCandidate"));
  assert.equal(body(mobile), body(desktop), "mobile/lib/receiptTarget.ts drifted from the desktop rule");
});
