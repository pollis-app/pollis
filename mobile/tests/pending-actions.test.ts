/* Acting on a message still being sent (#2141): reply, edit and delete (= discard) under the
 * core's id; reactions and pins wait until it is delivered. The core half is
 * src-tauri/tests/flows/outbox.rs; desktop's is e2e/chat-latency.spec.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { replyTargets } from "../lib/replyPreview.ts";
import { applyLocalChanges, restoreRow } from "../hooks/queries/pendingMessages.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const quotable = (id: string, extra: Record<string, unknown> = {}) => ({
  id,
  content: id,
  attachments: [],
  ...extra,
});

test("a reply to a stub finds its original by the core's id", () => {
  const stub = quotable("pending-1", { outboxId: "01CORE" });
  const reply = quotable("01REPLY", { reply_to_id: "01CORE" });
  const map = replyTargets([reply, stub]);
  assert.equal(map.get("01CORE"), stub);
  // Once stored, the original is found by its own id, as before.
  const stored = quotable("01CORE");
  assert.equal(replyTargets([reply, stored]).get("01CORE"), stored);
});

test("a pending row's sheet offers reply, copy, edit and delete only", () => {
  const sheet = src("components/chat/MessageActionsSheet.tsx");
  // Quick reactions, the picker, threads, copy link, save and pin sit behind `pending`.
  for (const id of ["btn-react-${ei}", "btn-reply-thread", "btn-react-more", "btn-copy-link", "btn-save", "btn-pin"]) {
    const at = sheet.indexOf(`testID${id.includes("$") ? "={`" : '="'}${id}`);
    assert.ok(at > 0, `${id} is on the sheet`);
    const guard = sheet.lastIndexOf("{pending ? null : (", at);
    const close = sheet.lastIndexOf("\n        )}\n", at);
    assert.ok(guard > 0 && guard > close, `${id} is hidden while pending`);
  }
  for (const id of ["btn-reply", "btn-copy-text", "btn-edit", "btn-delete"]) {
    assert.ok(sheet.includes(`testID="${id}"`), `${id} stays`);
  }
  assert.match(src("app/chat/[id].tsx"), /pending=\{!!actionTarget\.pending\}/);
});

test("delete on a pending row discards it, and edits name the core's id", () => {
  const chat = src("app/chat/[id].tsx");
  assert.match(chat, /if \(actionTarget\.pending\) \{\s*discardUnsent\.mutate\(actionTarget\.outboxId \?\? actionTarget\.id\);/);
  assert.match(chat, /messageId: target\.outboxId \?\? target\.id, newContent: text/);
  const hook = src("hooks/queries/useMessages.ts");
  // Edits find a pending row by the core's id through the overlay (`applyLocalChanges`).
  assert.match(hook, /drawLocalChange\(queryClient, key, vars\.messageId, change\)/);
  // The stub carries the pooled id from its first frame.
  assert.match(hook, /outboxId: named \?\? undefined,/);
});

// ── Review fixes (#2141) ──


test("an edit or delete in flight stays drawn over a refetch that read the old row", () => {
  const fetched = [quotable("01A"), quotable("pending-2", { outboxId: "01B", pending: true })];
  const changes = new Map([
    ["01A", (m: ReturnType<typeof quotable>) => ({ ...m, content: "edited" })],
    ["01B", (m: ReturnType<typeof quotable>) => ({ ...m, content: "", deleted_at: 1 })],
  ]);
  const rows = applyLocalChanges(fetched, changes);
  assert.equal(rows[0].content, "edited");
  assert.equal((rows[1] as { deleted_at?: number }).deleted_at, 1, "found by the stub's core id");
  // Nothing in flight: the very same rows.
  assert.equal(applyLocalChanges(fetched, new Map()), fetched);
});

test("a refused change puts back its one row and leaves the rest as they are now", () => {
  const before = quotable("01A");
  const now = [
    { ...quotable("01A"), content: "" },
    quotable("pending-9", { outboxId: "01N", failed: true }),
  ];
  const rows = restoreRow(now, "01A", before);
  assert.equal(rows[0], before);
  assert.equal(rows[1], now[1], "a send that failed meanwhile keeps its row");
});

test("edit and delete keep their change over refetches and never restore a whole page", () => {
  const hook = src("hooks/queries/useMessages.ts");
  for (const name of ["export function useEditMessage", "export function useDeleteMessage"]) {
    const body = hook.slice(hook.indexOf(name), hook.indexOf("\n}\n", hook.indexOf(name)));
    assert.ok(body.includes("drawLocalChange("), `${name} draws through the overlay`);
    assert.ok(body.includes("settleLocalChange("), `${name} settles it`);
    assert.ok(!/setQueryData\(ctx\.key, ctx\.previous\)/.test(body), `${name} restores no page snapshot`);
  }
  assert.match(hook, /applyLocalChanges\(\(page\.messages \?\? \[\]\)\.map\(transformMessage\), localChanges\)/);
});

test("a stub without the core's id offers no actions", () => {
  const item = src("components/chat/MessageRowItem.tsx");
  assert.match(item, /m\.pending === true && !!m\.outboxId/);
});
