/* Attachment key scope (#1382): the parser never hands Rust a scope for a reference it cannot open. */
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseContent } from "../lib/messageContent.ts";

const ref = (extra: Record<string, unknown>) =>
  JSON.stringify({
    _att: [{ key: "media/f1.enc", name: "a.png", ct: "image/png", size: 3, hash: "f1", ...extra }],
  });

test("a channel attachment names its conversation and key version", () => {
  const [a] = parseContent(ref({ kv: 2, sha: "d1g" }), { conversationId: "chan-1" }).attachments;
  assert.deepEqual(a.key_scope, { kind: "conversation", conversation_id: "chan-1", key_version: 2 });
  assert.equal(a.sha256, "d1g");
});

test("a vault attachment is version 0 under the vault key", () => {
  const [a] = parseContent(ref({ kv: 0, sha: "d1g" }), "vault").attachments;
  assert.deepEqual(a.key_scope, { kind: "vault" });
});

test("a pre-#1382 reference cannot be opened", () => {
  const [a] = parseContent(ref({}), { conversationId: "chan-1" }).attachments;
  assert.equal(a.key_scope, null);
});

test("a version-0 reference outside the vault, or a ring version inside it, is refused", () => {
  assert.equal(parseContent(ref({ kv: 0, sha: "d" }), { conversationId: "c" }).attachments[0].key_scope, null);
  assert.equal(parseContent(ref({ kv: 1, sha: "d" }), "vault").attachments[0].key_scope, null);
});

test("a preview-only surface never produces a fetchable scope", () => {
  assert.equal(parseContent(ref({ kv: 1, sha: "d" }), null).attachments[0].key_scope, null);
});
