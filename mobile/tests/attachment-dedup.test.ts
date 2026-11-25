// #2109: shared attachment references (`v: 2`) — parsing, the envelope's key fields, the Vault
// edit that keeps them, and the "File removed" / "update to open" states. Pure.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { parseContent, referenceKeyFields } from "../lib/messageContent.ts";
import { editedVaultContent } from "../lib/vault.ts";
import { isAttachmentRemoved, mediaProblem } from "../lib/media/removed.ts";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const ID = `v2-${"ab".repeat(32)}`;
const SHA = "cd".repeat(32);
const v2 = (extra: Record<string, unknown> = {}) =>
  JSON.stringify({
    _att: [{ key: `media/${ID}.enc`, url: "", name: "cat.png", ct: "image/png", size: 3, hash: ID, v: 2, sha: SHA, ...extra }],
    _txt: "look",
  });

test("a v2 reference opens under its own digest in a chat and in the Vault", () => {
  for (const origin of [{ conversationId: "c1" }, "vault" as const]) {
    const [a] = parseContent(v2(), origin).attachments;
    assert.deepEqual(a.key_scope, { kind: "content" });
    assert.equal(a.version, 2);
    assert.equal(a.content_hash, ID);
    assert.equal(a.needs_update, undefined);
  }
});

test("a preview-only origin still opens nothing", () => {
  const [a] = parseContent(v2(), null).attachments;
  assert.equal(a.key_scope, null);
});

test("a v1 reference keeps its ring key scope", () => {
  const raw = JSON.stringify({ _att: [{ key: "media/x.enc", name: "a", ct: "image/png", size: 1, hash: "ab".repeat(32), kv: 3, sha: SHA }] });
  const [a] = parseContent(raw, { conversationId: "c1" }).attachments;
  assert.deepEqual(a.key_scope, { kind: "conversation", conversation_id: "c1", key_version: 3 });
  assert.equal(a.version, 1);
});

test("a reference from a newer app version is shown as update-to-open, never fetched", () => {
  const [a] = parseContent(v2({ v: 3 }), { conversationId: "c1" }).attachments;
  assert.equal(a.key_scope, null);
  assert.equal(a.needs_update, true);
  assert.equal(mediaProblem(a, null), "update");
});

test("the envelope carries v: 2 and no kv for a shared object, kv for v1", () => {
  assert.deepEqual(referenceKeyFields(2, 0), { v: 2 });
  assert.deepEqual(referenceKeyFields(undefined, 0), { kv: 0 });
  assert.deepEqual(referenceKeyFields(1, 4), { kv: 4 });
});

test("a Vault text edit keeps a shared file's v: 2", () => {
  const edited = editedVaultContent(v2(), "new caption");
  assert.ok(edited);
  const parsed = JSON.parse(edited);
  assert.equal(parsed._att[0].v, 2);
  assert.equal("kv" in parsed._att[0], false);
  assert.equal(parsed._txt, "new caption");
});

test("the core's removed error is recognised exactly", () => {
  assert.equal(isAttachmentRemoved("attachment_removed"), true);
  assert.equal(isAttachmentRemoved(new Error("bridge: attachment_removed")), true);
  assert.equal(isAttachmentRemoved("R2 download failed: 404"), false);
  assert.equal(isAttachmentRemoved("the attachment_removed flag was mentioned"), false);
  assert.equal(mediaProblem({}, "attachment_removed"), "removed");
  assert.equal(mediaProblem({}, new Error("timeout")), "unavailable");
  assert.equal(mediaProblem({}, null), null);
});

test("mobile's envelope helper matches desktop's byte for byte", () => {
  const body = (s: string) => {
    const start = s.indexOf("export function referenceKeyFields");
    return s.slice(start, s.indexOf("}\n", start) + 2);
  };
  assert.equal(
    body(src("lib/messageContent.ts")),
    body(readFileSync(new URL("../../frontend/src/utils/attachmentEnvelope.ts", import.meta.url), "utf8")),
  );
});
