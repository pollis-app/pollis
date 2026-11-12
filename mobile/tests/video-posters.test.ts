/* Video posters (#1837): `pv` reaches the attachment, a bad one is dropped, an old message has none, and the tile's source order. */
import { test } from "node:test";
import assert from "node:assert/strict";
import { parseContent, posterOf, posterUri } from "../lib/messageContent.ts";
import { editedVaultContent } from "../lib/vault.ts";
import { LEGACY_POSTER_MAX_BYTES, videoPosterPlan } from "../lib/media/videoPoster.ts";

const JPEG = "/9j/4AAQSkZJRgABAQ==";

const entry = (key: string, extra: Record<string, unknown> = {}) => ({
  key,
  name: "clip.mp4",
  ct: "video/mp4",
  size: 3,
  hash: `h-${key}`,
  kv: 1,
  sha: "s",
  bh: "LEHV6nWB2yk8pyo0adR*.7kCMdnj",
  w: 1280,
  h: 720,
  ...extra,
});

test("pv becomes the attachment's poster", () => {
  const [att] = parseContent(JSON.stringify({ _att: [entry("media/a.enc", { pv: JPEG })] }), {
    conversationId: "c1",
  }).attachments;
  assert.equal(att.poster, JPEG);
  assert.equal(posterUri(att.poster!), `data:image/jpeg;base64,${JPEG}`);
});

test("a message from an older sender (no pv) parses exactly as before", () => {
  const [att] = parseContent(JSON.stringify({ _att: [entry("media/a.enc")] }), {
    conversationId: "c1",
  }).attachments;
  assert.equal(att.poster, undefined);
  assert.equal(att.blurhash, "LEHV6nWB2yk8pyo0adR*.7kCMdnj");
  assert.equal(att.width, 1280);
});

test("anything but plain, bounded base64 is ignored", () => {
  assert.equal(posterOf(JPEG), JPEG);
  assert.equal(posterOf(""), undefined);
  assert.equal(posterOf(42), undefined);
  assert.equal(posterOf({ src: JPEG }), undefined);
  assert.equal(posterOf('x" onerror="alert(1)'), undefined);
  assert.equal(posterOf("javascript:alert(1)"), undefined);
  assert.equal(posterOf("A".repeat(64 * 1024 + 4)), undefined);
});

test("a Vault text edit keeps the poster", () => {
  const original = JSON.stringify({ _att: [entry("media/a.enc", { kv: 0, pv: JPEG })], _txt: "old" });
  const edited = JSON.parse(editedVaultContent(original, "new")!);
  assert.equal(edited._att[0].pv, JPEG);
  assert.equal(edited._txt, "new");
});

test("a tile's frame: pv first, then a pending send's file, then a small or already-local legacy file", () => {
  const base = { object_key: "media/a.enc", localPreviewUri: undefined, file_size: 1024 };
  assert.equal(videoPosterPlan({ ...base, poster: JPEG }, false), "pv");
  assert.equal(videoPosterPlan({ ...base, object_key: "", localPreviewUri: "file:///v.mp4" }, false), "pending");
  assert.equal(videoPosterPlan({ ...base, object_key: "" }, false), "none");
  assert.equal(videoPosterPlan(base, false), "legacy");
  const big = { ...base, file_size: LEGACY_POSTER_MAX_BYTES + 1 };
  assert.equal(videoPosterPlan(big, false), "none", "no download just for a frame");
  assert.equal(videoPosterPlan(big, true), "legacy", "already on disk costs nothing");
});
