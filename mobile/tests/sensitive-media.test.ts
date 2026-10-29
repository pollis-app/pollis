/*
 * Sensitive media, message side (#1568).
 *
 * The `_att` entry's `sens: true` (inside the encrypted envelope) reaches the
 * attachment; a covered tile's tap reveals and does nothing else, so the tap
 * that uncovers a video never opens the viewer or starts it playing; the
 * reveal is per attachment, shared by every surface, and an attachment still
 * uploading keeps its reveal to itself. A Vault caption edit keeps the flag.
 * The rendering itself is the Maestro tier's.
 *
 *   node --test mobile/tests/
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { parseContent } from "../lib/messageContent.ts";
import { isCovered, revealKey, tileTap } from "../lib/sensitiveMedia.ts";
import { sensitiveRevealStore } from "../stores/sensitiveRevealStore.ts";
import { editedVaultContent } from "../lib/vault.ts";

const entry = (key: string, ct: string, extra: Record<string, unknown> = {}) => ({
  key,
  name: key.split("/").pop(),
  ct,
  size: 3,
  hash: `h-${key}`,
  kv: 1,
  sha: "s",
  bh: "LEHV6nWB2yk8pyo0adR*.7kCMdnj",
  ...extra,
});

const content = JSON.stringify({
  _att: [
    entry("media/a.enc", "image/jpeg", { sens: true }),
    entry("media/b.enc", "image/jpeg", { sens: true }),
    entry("media/clip.enc", "video/mp4", { sens: true }),
    entry("media/plain.enc", "image/jpeg"),
  ],
  _txt: "careful",
});

const parsed = () => parseContent(content, { conversationId: "c1" }).attachments;

test("sens marks an attachment sensitive; its absence (an older sender) does not", () => {
  const [a, b, clip, plain] = parsed();
  assert.equal(a.sensitive, true);
  assert.equal(b.sensitive, true);
  assert.equal(clip.sensitive, true);
  assert.equal(plain.sensitive, false);
});

test("a covered tile's tap reveals; it never opens — a video does not play on it", () => {
  const [, , clip, plain] = parsed();
  assert.equal(isCovered(clip, false), true);
  assert.equal(tileTap(clip, false), "reveal");
  // Revealed, a second tap opens the viewer as for any video.
  assert.equal(tileTap(clip, true), "open");
  // An unmarked attachment opens on the first tap, as before.
  assert.equal(isCovered(plain, false), false);
  assert.equal(tileTap(plain, false), "open");
});

test("revealing one attachment reveals only that one, and hiding covers it again", () => {
  sensitiveRevealStore.clear();
  const [a, b, clip] = parsed();
  sensitiveRevealStore.reveal(revealKey(a)!);
  assert.equal(sensitiveRevealStore.isRevealed(a.id), true);
  assert.equal(sensitiveRevealStore.isRevealed(b.id), false);
  assert.equal(sensitiveRevealStore.isRevealed(clip.id), false);
  // The same attachment parsed again (the viewer, the Vault grid) agrees.
  const again = parsed()[0];
  assert.equal(sensitiveRevealStore.isRevealed(revealKey(again)!), true);
  sensitiveRevealStore.hide(a.id);
  assert.equal(sensitiveRevealStore.isRevealed(a.id), false);
  sensitiveRevealStore.clear();
});

test("an attachment still uploading has no shared reveal key", () => {
  const pending = parseContent(
    JSON.stringify({ _att: [{ name: "a.jpg", ct: "image/jpeg", size: 3, sens: true }] }),
    { conversationId: "c1" },
  ).attachments[0];
  assert.equal(pending.sensitive, true);
  assert.equal(revealKey(pending), null);
});

test("a Vault caption edit keeps the flag", () => {
  const vault = JSON.stringify({
    _att: [entry("media/a.enc", "image/jpeg", { kv: 0, sens: true }), entry("media/b.enc", "image/jpeg", { kv: 0 })],
    _txt: "old",
  });
  const edited = JSON.parse(editedVaultContent(vault, "new")!);
  assert.equal(edited._att[0].sens, true);
  assert.equal("sens" in edited._att[1], false);
});

test("the message tile, the viewer and the Vault grid all read the reveal", () => {
  for (const file of [
    "components/chat/AttachmentTile.tsx",
    "components/media/MediaViewerPage.tsx",
    "components/vault/VaultMediaGrid.tsx",
  ]) {
    const src = readFileSync(new URL(`../${file}`, import.meta.url), "utf8");
    assert.match(src, /useSensitiveReveal\(attachment\)/, file);
    assert.match(src, /observer\(/, file);
    assert.match(src, /SensitiveCover/, file);
  }
  // The covered tile's press is the reveal, never `onOpen`.
  const tile = readFileSync(new URL("../components/chat/AttachmentTile.tsx", import.meta.url), "utf8");
  const covered = tile.slice(tile.indexOf('=== "reveal"'), tile.indexOf("const withHide"));
  assert.match(covered, /onPress=\{sensitivity\.reveal\}/);
  assert.doesNotMatch(covered, /onOpen/);
});
