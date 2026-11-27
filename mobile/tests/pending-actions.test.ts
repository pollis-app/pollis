/* Acting on a message still being sent (#2141): reply, edit and delete (= discard) under the
 * core's id; reactions and pins wait until it is delivered. The core half is
 * src-tauri/tests/flows/outbox.rs; desktop's is e2e/chat-latency.spec.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { replyTargets } from "../lib/replyPreview.ts";

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
  assert.match(hook, /m\.id === vars\.messageId \|\| m\.outboxId === vars\.messageId/);
  // The stub carries the pooled id from its first frame.
  assert.match(hook, /outboxId: named \?\? undefined,/);
});
