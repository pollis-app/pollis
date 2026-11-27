/* Edits and deletes in flight (#2141): kept drawn over refetches, and a refusal puts back
 * only its own row. The hook wiring is in src/hooks/queries/useMessages.ts. */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { applyLocalChanges, restoreRow } from "../src/utils/pendingMessages.ts";

type Row = { id: string; outbox_id?: string; status?: "failed"; content_decrypted?: string; deleted_at?: string };

test("an edit or delete in flight stays drawn over a refetch that read the old row", () => {
  const fetched: Row[] = [{ id: "01A", content_decrypted: "old" }, { id: "pending-2", outbox_id: "01B" }];
  const rows = applyLocalChanges(fetched, new Map([
    ["01A", (m: Row) => ({ ...m, content_decrypted: "edited" })],
    ["01B", (m: Row) => ({ ...m, deleted_at: "now" })],
  ]));
  assert.equal(rows[0].content_decrypted, "edited");
  assert.equal(rows[1].deleted_at, "now", "found by the stub's core id");
  assert.equal(applyLocalChanges(fetched, new Map()), fetched);
});

test("a refused change puts back its one row and leaves the rest as they are now", () => {
  const before: Row = { id: "01A", content_decrypted: "old" };
  const now: Row[] = [{ id: "01A", deleted_at: "now" }, { id: "pending-9", outbox_id: "01N", status: "failed" }];
  const rows = restoreRow(now, "01A", before);
  assert.equal(rows[0], before);
  assert.equal(rows[1], now[1], "a send that failed meanwhile keeps its row and Retry");
});

test("delete and edit touch only their own conversation and keep their change until settled", () => {
  const hook = readFileSync(new URL("../src/hooks/queries/useMessages.ts", import.meta.url), "utf8");
  const del = hook.slice(hook.indexOf("export function useDeleteMessage"), hook.indexOf("type EditMessageVars"));
  assert.ok(!del.includes("messageQueryKeys.all"), "no snapshot of every conversation");
  assert.ok(del.includes("drawLocalChange(") && del.includes("settleLocalChange("));
  const edit = hook.slice(hook.indexOf("export function useEditMessage"));
  assert.ok(edit.includes("drawLocalChange(") && !edit.includes("previousData"));
  assert.match(hook, /applyLocalChanges\(\s*carryStubs\(/);
});

test("a stub without the core's id offers no actions", () => {
  const item = readFileSync(new URL("../src/components/Message/MessageItem.tsx", import.meta.url), "utf8");
  assert.match(item, /!\(isUnsent && \(isFailed \|\| !message\.outbox_id\)\)/);
});
