/* #1735: which history transfers run, what each state shows, what a retry does, and how the device list orders. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  currentReceiverTransfer,
  linkOfferSource,
  needsRun,
  retryKind,
  sortSources,
  transferPhase,
  upsertTransfer,
} from "../src/utils/historyTransfer.ts";
import type { HistoryTransfer, TransferSource } from "../src/services/api.ts";

function transfer(over: Partial<HistoryTransfer> = {}): HistoryTransfer {
  return {
    transfer_id: "t1",
    role: "receiver",
    peer_device_id: "d1",
    peer_device_name: "Mac",
    via: "sibling",
    state: "transferring",
    chunks_done: 0,
    rows_done: 0,
    rows_skipped: 0,
    deferred_conversations: 0,
    updated_at: "2026-11-08T10:00:00Z",
    ...over,
  };
}

function source(over: Partial<TransferSource> = {}): TransferSource {
  return { device_id: "d", device_name: "D", online: false, ...over };
}

test("every unfinished transfer runs, except a request the source has not answered", () => {
  assert.equal(needsRun(transfer({ state: "transferring" })), true);
  assert.equal(needsRun(transfer({ state: "paused" })), true);
  assert.equal(needsRun(transfer({ state: "awaiting_approval" })), true);
  assert.equal(needsRun(transfer({ role: "source", state: "awaiting_approval" })), false);
  for (const state of ["completed", "declined", "cancelled", "expired", "failed"] as const) {
    assert.equal(needsRun(transfer({ state })), false, state);
  }
});

test("a completed copy with conversations left behind reads as deferred", () => {
  assert.equal(transferPhase(transfer({ state: "completed" })), "done");
  assert.equal(transferPhase(transfer({ state: "completed", deferred_conversations: 2 })), "deferred");
  assert.equal(transferPhase(transfer({ state: "awaiting_approval" })), "waiting");
});

test("pause resumes; only the receiver asks again", () => {
  assert.equal(retryKind(transfer({ state: "paused" }), false), "resume");
  assert.equal(retryKind(transfer({ state: "paused" }), true), null);
  assert.equal(retryKind(transfer({ state: "failed" }), false), "request");
  assert.equal(retryKind(transfer({ state: "completed", deferred_conversations: 1 }), false), "request");
  assert.equal(retryKind(transfer({ state: "completed" }), false), null);
  assert.equal(retryKind(transfer({ role: "source", state: "failed" }), false), null);
  assert.equal(retryKind(transfer({ role: "source", state: "paused" }), false), "resume");
});

test("devices order online first, then by last activity; ones that can't send go last", () => {
  const sorted = sortSources([
    source({ device_id: "old", online: true, supports_transfer: false }),
    source({ device_id: "idle", last_active_at: "2026-11-01T00:00:00Z" }),
    source({ device_id: "recent", last_active_at: "2026-11-07T00:00:00Z" }),
    source({ device_id: "on", online: true }),
  ]);
  assert.deepEqual(sorted.map((s) => s.device_id), ["on", "recent", "idle", "old"]);
});

test("the link offer names the approver, or the only device that can send, else asks", () => {
  assert.equal(
    linkOfferSource([source({ device_id: "a" }), source({ device_id: "b", approved_this_device: true })])?.device_id,
    "b",
  );
  assert.equal(
    linkOfferSource([source({ device_id: "a" }), source({ device_id: "b", supports_transfer: false })])?.device_id,
    "a",
  );
  assert.equal(linkOfferSource([source({ device_id: "a" }), source({ device_id: "b" })]), null);
});

test("the receiver screen follows the newest copy still under way", () => {
  const list = [
    transfer({ transfer_id: "done", state: "completed", updated_at: "2026-11-08T12:00:00Z" }),
    transfer({ transfer_id: "src", role: "source", updated_at: "2026-11-08T12:00:00Z" }),
    transfer({ transfer_id: "open", state: "paused", updated_at: "2026-11-08T11:00:00Z" }),
  ];
  assert.equal(currentReceiverTransfer(list)?.transfer_id, "open");
  assert.equal(currentReceiverTransfer([list[0]]), null);
});

test("a progress frame replaces its transfer by id", () => {
  const list = upsertTransfer([transfer({ transfer_id: "a" }), transfer({ transfer_id: "b" })], transfer({ transfer_id: "b", rows_done: 9 }));
  assert.deepEqual(list.map((t) => [t.transfer_id, t.rows_done]), [["b", 9], ["a", 0]]);
});
