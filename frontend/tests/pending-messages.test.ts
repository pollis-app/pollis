// The outbox's renderer half (#1335): one row per send, never two; a failed one keeps a way out.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  carryStubs,
  isLocalStub,
  mapOutboxRows,
  outboxFields,
  retryTarget,
  settleSend,
  withoutOutboxRows,
  type OutgoingLike,
} from "../src/utils/pendingMessages.ts";

const row = (id: string, extra: Partial<OutgoingLike> = {}): OutgoingLike => ({ id, ...extra });

test("a stored row's send_state maps to the row's status", () => {
  assert.deepEqual(outboxFields("01A", "pending"), { status: "sending", outbox_id: "01A", delivered: false });
  assert.deepEqual(outboxFields("01A", "failed"), { status: "failed", outbox_id: "01A", delivered: false });
  assert.deepEqual(outboxFields("01A", undefined), { status: "sent", delivered: true });
  // A refusal no retry fixes is shown like any failure, Retry included.
  assert.deepEqual(outboxFields("01A", "rejected"), { status: "failed", outbox_id: "01A", delivered: false });
  assert.equal(retryTarget(row("01A", outboxFields("01A", "rejected"))), "01A");
});

test("a refetch keeps a stub the store does not hold, and drops one it does", () => {
  // In flight, not stored yet: carried at the end of the oldest-first log.
  const inFlight = row("pending-1", { status: "sending", outbox_id: "01A" });
  assert.deepEqual(carryStubs([row("a")], [row("a"), inFlight]).map((m) => m.id), ["a", "pending-1"]);
  // Stored (pending) under the stub's id: one row, the stored one.
  const stored = row("01A", outboxFields("01A", "pending"));
  assert.deepEqual(carryStubs([row("a"), stored], [row("a"), inFlight]).map((m) => m.id), ["a", "01A"]);
  // Failed before the core stored it: stays, Discard-only.
  const orphan = row("pending-2", { status: "failed", outbox_id: "01B" });
  const carried = carryStubs([row("a")], [orphan]);
  assert.deepEqual(carried.map((m) => m.id), ["a", "pending-2"]);
  assert.equal(carried[1].retryable, false);
  assert.equal(retryTarget(carried[1]), null);
  // An in-flight stub is carried as it is.
  assert.equal(carryStubs([row("a")], [inFlight])[1].retryable, undefined);
  assert.equal(isLocalStub(stored), false);
});

test("a settling send never leaves two copies", () => {
  const confirmed = row("01A", { status: "sent" });
  const stub = row("pending-1", { status: "sending", outbox_id: "01A" });
  const storedPending = row("01A", outboxFields("01A", "pending"));
  // Stub only: swapped in place.
  assert.deepEqual(settleSend([row("a"), stub, row("b")], "pending-1", confirmed).map((m) => m.id), ["a", "01A", "b"]);
  // A refetch already brought the stored row in, and the stub is still there.
  const both = settleSend([row("a"), storedPending, stub], "pending-1", confirmed);
  assert.deepEqual(both.map((m) => m.id), ["a", "01A"]);
  assert.equal(both[1].status, "sent");
  // Neither: appended.
  assert.deepEqual(settleSend([row("a")], "pending-1", confirmed).map((m) => m.id), ["a", "01A"]);
});

test("Retry is offered only for a failed message the core stored", () => {
  assert.equal(retryTarget(row("01A", outboxFields("01A", "failed"))), "01A");
  assert.equal(retryTarget(row("pending-1", { status: "failed", outbox_id: "01A" })), "01A");
  assert.equal(retryTarget(row("pending-1", { status: "failed", outbox_id: "01A", retryable: false })), null);
  assert.equal(retryTarget(row("01A", outboxFields("01A", "pending"))), null);
  assert.equal(retryTarget(row("pending-1", { status: "failed" })), null);
});

test("retry and discard reach every copy of the message", () => {
  const rows = [
    row("pending-1", { status: "failed", outbox_id: "01A" }),
    row("01A", outboxFields("01A", "failed")),
    row("b"),
  ];
  const retried = mapOutboxRows(rows, "01A", (m) => ({ ...m, status: "sending" as const }));
  assert.deepEqual(retried.map((m) => m.status), ["sending", "sending", undefined]);
  assert.deepEqual(withoutOutboxRows(rows, "01A").map((m) => m.id), ["b"]);
});

test("every send names its message and the shell drains the outbox on events only", () => {
  const src = (p: string) => readFileSync(new URL(`../src/${p}`, import.meta.url), "utf8");
  const main = src("components/Layout/MainContent.tsx");
  assert.match(main, /invoke<string>\("new_message_id"\)[\s\S]*outgoingAttachments\(attachments\)[\s\S]*attachments: outgoing,[\s\S]*messageId,\n\s*turn,\n\s*\}\);/);
  // The upload belongs to the outbox now (#1335): neither composer uploads itself.
  assert.doesNotMatch(main, /buildMessageContent\(/);
  assert.doesNotMatch(src("components/Layout/RightPanel/ThreadPanel.tsx"), /buildMessageContent\(/);
  const hooks = src("hooks/queries/useMessages.ts");
  assert.match(hooks, /messageId: id,/);
  assert.match(src("components/Layout/AppShell.tsx"), /flushOutbox\(queryClient\)[\s\S]*addEventListener\("online"/);
  assert.match(src("hooks/useLiveKitRealtime.ts"), /realtime_reconnected[\s\S]*flushOutbox\(/);
});
