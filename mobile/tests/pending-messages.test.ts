import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  carryStubs,
  isLocalStub,
  mapOutboxRows,
  retryTarget,
  settleSend,
  unsentFlags,
  withoutOutboxRows,
  type StubLike,
} from "../hooks/queries/pendingMessages.ts";

const row = (id: string, extra: { pending?: boolean; failed?: boolean } = {}) => ({ id, ...extra });

test("only local optimistic rows count as stubs", () => {
  assert.equal(isLocalStub(row("pending-1", { pending: true })), true);
  assert.equal(isLocalStub(row("pending-1", { failed: true })), true);
  assert.equal(isLocalStub(row("pending-1")), false);
  assert.equal(isLocalStub(row("01ABC", { pending: true })), false);
});

test("a refetch mid-send keeps the stub at the new end", () => {
  const cached = [row("pending-9", { pending: true }), row("b"), row("a")];
  assert.deepEqual(carryStubs([row("b"), row("a")], cached).map((m) => m.id), ["pending-9", "b", "a"]);
  // A thread is oldest-first: the stub stays last.
  assert.deepEqual(
    carryStubs([row("a"), row("b")], [row("a"), row("b"), row("pending-9", { pending: true })], "oldestFirst").map((m) => m.id),
    ["a", "b", "pending-9"],
  );
  // A failed stub is kept too, so a failed send stays labelled, never vanishes.
  assert.deepEqual(carryStubs([row("a")], [row("pending-3", { failed: true })]).map((m) => m.id), ["pending-3", "a"]);
  const fetched = [row("a")];
  assert.equal(carryStubs(fetched, [row("a")]), fetched);
  assert.equal(carryStubs(fetched, undefined), fetched);
});

test("a settling send swaps the stub, or restores the row if a refetch lost it", () => {
  const confirmed = row("01NEW");
  assert.deepEqual(settleSend([row("pending-9", { pending: true }), row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
  // The stub was dropped by an earlier refetch.
  assert.deepEqual(settleSend([row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
  assert.deepEqual(settleSend([row("a")], "pending-9", confirmed, "oldestFirst").map((m) => m.id), ["a", "01NEW"]);
  // A refetch already brought the confirmed row in: no duplicate.
  assert.deepEqual(settleSend([row("01NEW"), row("a")], "pending-9", confirmed).map((m) => m.id), ["01NEW", "a"]);
});

test("both message queries carry stubs and both settle paths use settleSend", () => {
  const s = readFileSync(new URL("../hooks/queries/useMessages.ts", import.meta.url), "utf8");
  assert.equal(s.match(/carryStubs\(/g)?.length, 2);
  assert.equal(s.match(/settleSend\(/g)?.length, 2);
  assert.match(s, /settleSend\(p\.messages, ctx\.optimisticId, settled\)[\s\S]{0,400}invalidateQueries\(\{ queryKey: ctx\.key \}\)/);
});

test("a stub steps aside once the store holds its message", () => {
  // The store holds the stub's message: one row, not two.
  const stub = { id: "pending-9", pending: true, outboxId: "01MSG" };
  const fetched = [{ id: "01MSG", pending: true, outboxId: "01MSG" }, row("a")];
  assert.deepEqual(carryStubs(fetched, [stub, row("a")]).map((m) => m.id), ["01MSG", "a"]);
  // A failed stub the store holds as failed: the stored row is kept.
  const failedStub = { id: "pending-9", failed: true, outboxId: "01MSG" };
  const storedFailed = [{ id: "01MSG", failed: true, outboxId: "01MSG" }];
  assert.deepEqual(carryStubs(storedFailed, [failedStub]).map((m) => m.id), ["01MSG"]);
  // A stub the store never got is carried as Discard-only.
  const orphan = { id: "pending-7", failed: true, outboxId: "01GONE" };
  const carried = carryStubs<StubLike>([row("a")], [orphan]);
  assert.deepEqual(carried.map((m) => m.id), ["pending-7", "a"]);
  assert.equal(carried[0].retryable, false);
  assert.equal(retryTarget(carried[0]), null);
  // An in-flight stub is carried as it is.
  const inFlight = { id: "pending-8", pending: true, outboxId: "01SENDING" };
  assert.equal(carryStubs<StubLike>([row("a")], [inFlight])[0].retryable, undefined);
});

test("a settling send replaces a stored pending row instead of duplicating it", () => {
  const confirmed = row("01MSG");
  const rows = [
    { id: "pending-9", pending: true, outboxId: "01MSG" },
    { id: "01MSG", pending: true, outboxId: "01MSG" },
    row("a"),
  ];
  const settled = settleSend(rows, "pending-9", confirmed);
  assert.deepEqual(settled.map((m) => m.id), ["01MSG", "a"]);
  assert.equal(settled[0].pending, undefined, "the delivered row no longer reads pending");
});

test("a stored row's send_state maps to the stub flags", () => {
  assert.deepEqual(unsentFlags("01A", "pending"), { pending: true, outboxId: "01A" });
  assert.deepEqual(unsentFlags("01A", "failed"), { failed: true, outboxId: "01A" });
  assert.deepEqual(unsentFlags("01A", "rejected"), { failed: true, outboxId: "01A" });
  assert.equal(retryTarget({ id: "01A", ...unsentFlags("01A", "rejected") }), "01A");
  assert.deepEqual(unsentFlags("01A", undefined), {});
  assert.deepEqual(unsentFlags("01A", null), {});
  assert.equal(isLocalStub({ id: "01A", ...unsentFlags("01A", "pending") }), false);
});

test("Retry is offered only for a failed message the core stored", () => {
  assert.equal(retryTarget({ id: "01A", failed: true, outboxId: "01A" }), "01A");
  assert.equal(retryTarget({ id: "pending-1", failed: true, outboxId: "01A" }), "01A");
  // The upload failed before the core stored anything: discard only.
  assert.equal(retryTarget({ id: "pending-1", failed: true, outboxId: "01A", retryable: false }), null);
  assert.equal(retryTarget({ id: "01A", pending: true, outboxId: "01A" }), null);
  assert.equal(retryTarget({ id: "pending-1", failed: true }), null);
});

test("retry and discard act on every row standing for the message", () => {
  type Row = { id: string; pending?: boolean; failed?: boolean; outboxId?: string };
  const rows: Row[] = [
    { id: "pending-1", failed: true, outboxId: "01A" },
    { id: "01A", failed: true, outboxId: "01A" },
    row("b"),
  ];
  const retried = mapOutboxRows(rows, "01A", (m) => ({ ...m, failed: false, pending: true }));
  assert.deepEqual(retried.map((m) => m.pending === true), [true, true, false]);
  assert.deepEqual(withoutOutboxRows(rows, "01A").map((m) => m.id), ["b"]);
});

test("every send names its message and every surface drains the outbox", () => {
  const s = readFileSync(new URL("../hooks/queries/useMessages.ts", import.meta.url), "utf8");
  assert.match(s, /invoke<string>\("new_message_id"\)[\s\S]*attachments: outgoingAttachments\(vars\.attachments \?\? \[\]\)[\s\S]*messageId,\n\s*\}\);/);
  // The upload belongs to the outbox now (#1335): the send never uploads itself.
  assert.doesNotMatch(s.slice(s.indexOf("export function useSendMessage"), s.indexOf("export function useRetryMessage")), /buildMessageContent\(/);
  const inbox = readFileSync(new URL("../hooks/useInboxRealtime.ts", import.meta.url), "utf8");
  assert.equal(inbox.match(/flushOutbox\(/g)?.length, 3);
  assert.doesNotMatch(inbox, /setInterval/);
});

test("an attachment the outbox has not uploaded yet is drawn pending, never fetched", async () => {
  const { parseContent } = await import("../lib/messageContent.ts");
  const provisional = JSON.stringify({ _att: [{ name: "shot.png", ct: "image/png", size: 5 }], _txt: "look" });
  const { text, attachments } = parseContent(provisional, { conversationId: "c1" });
  assert.equal(attachments[0].key_scope, null, "an unuploaded entry has no key to fetch with");
  assert.equal(text, "look");
  assert.equal(attachments.length, 1);
  assert.equal(attachments[0].object_key, "");
  assert.equal(attachments[0].id, "uploading-0");
  assert.equal(attachments[0].filename, "shot.png");
  const media = readFileSync(new URL("../components/Media.tsx", import.meta.url), "utf8");
  assert.match(media, /const isPending = !attachment\.object_key;/);
});

test("picked attachments go to the core as paths, un-uploaded", () => {
  const s = readFileSync(new URL("../lib/attachments.ts", import.meta.url), "utf8");
  assert.match(s, /export function outgoingAttachments[\s\S]*path: att\.uri,[\s\S]*filename: att\.name,[\s\S]*contentType: att\.mimeType/);
});
