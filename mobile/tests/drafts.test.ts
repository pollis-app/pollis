/* Composer drafts: desktop's store, copied verbatim, and its key scheme. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { clearAllDrafts, getDraft, setDraft } from "../lib/drafts.ts";

const here = dirname(fileURLToPath(import.meta.url));
const read = (p: string) => readFileSync(join(here, p), "utf8");

test("mobile/lib/drafts.ts is desktop's drafts.ts, byte for byte", () => {
  assert.equal(
    read("../lib/drafts.ts"),
    read("../../frontend/src/utils/drafts.ts"),
    "edit frontend/src/utils/drafts.ts and copy it to mobile/lib/drafts.ts",
  );
});

test("a draft survives until it is emptied or the session ends", () => {
  setDraft("channel:a", "half a thought");
  setDraft("conv:b", "other room");
  assert.equal(getDraft("channel:a"), "half a thought");
  assert.equal(getDraft("conv:b"), "other room");
  setDraft("channel:a", "");
  assert.equal(getDraft("channel:a"), "");
  clearAllDrafts();
  assert.equal(getDraft("conv:b"), "");
});

test("no key, no draft", () => {
  setDraft(null, "lost");
  assert.equal(getDraft(null), "");
});

test("every composer screen keeps its draft under desktop's key", () => {
  const chat = read("../app/chat/[id].tsx");
  assert.ok(chat.includes("useDraft(conversationDraftKey(kind, conversationId))"));
  assert.ok(read("../app/chat/thread.tsx").includes("useDraft(threadId ? `thread:${threadId}` : null)"));
  assert.ok(read("../app/vault.tsx").includes('useDraft("vault")'));
  const hook = read("../hooks/useDraft.ts");
  assert.ok(hook.includes("`channel:${conversationId}`") && hook.includes("`conv:${conversationId}`"));
});
