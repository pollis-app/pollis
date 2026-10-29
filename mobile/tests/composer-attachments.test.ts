/*
 * Composer attachment previews (#1585): mobile's port of desktop's
 * `AttachmentPreview`. The rules are pure (lib/composerAttachments.ts) and
 * pinned directly; the card and toolbar are pinned at the source level, as
 * the RN renderer is not available under node.
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  COMPOSER_PREVIEW_SIZE,
  canOpenPreview,
  composerAttachmentTestID,
  draftViewerItems,
  getDraftForViewer,
  pickedFromAssets,
  pickedToMessageAttachment,
  previewKind,
  removeAttachmentTestID,
  setDraftForViewer,
} from "../lib/composerAttachments.ts";
import type { PickedAttachment } from "../lib/attachments.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (rel: string) => readFileSync(join(ROOT, rel), "utf8");

const img = (id: string): PickedAttachment => ({
  id,
  uri: `file:///tmp/${id}.jpg`,
  name: `${id}.jpg`,
  mimeType: "image/jpeg",
  width: 400,
  height: 300,
});
const vid = (id: string): PickedAttachment => ({
  id,
  uri: `file:///tmp/${id}.mp4`,
  name: `${id}.mp4`,
  mimeType: "video/mp4",
});
const pdf = (id: string): PickedAttachment => ({
  id,
  uri: `file:///tmp/${id}.pdf`,
  name: `${id}.pdf`,
  mimeType: "application/pdf",
});

test("the card is desktop's 80px preview taken up by half, as #1568 does", () => {
  assert.equal(COMPOSER_PREVIEW_SIZE, 120);
});

test("a card draws by MIME type, and only images and videos open the viewer", () => {
  assert.equal(previewKind("image/png"), "image");
  assert.equal(previewKind("video/quicktime"), "video");
  assert.equal(previewKind("audio/mpeg"), "audio");
  assert.equal(previewKind("application/pdf"), "file");
  assert.equal(canOpenPreview("image/heic"), true);
  assert.equal(canOpenPreview("video/mp4"), true);
  assert.equal(canOpenPreview("audio/mpeg"), false);
  assert.equal(canOpenPreview("application/pdf"), false);
});

test("test ids follow the strip position", () => {
  assert.equal(composerAttachmentTestID(0), "composer-attachment-0");
  assert.equal(removeAttachmentTestID(2), "btn-remove-attachment-2");
});

test("a picked file becomes a keyless attachment that draws from its local file", () => {
  const a = pickedToMessageAttachment(img("a"), 42);
  assert.equal(a.id, "a");
  assert.equal(a.object_key, "");
  assert.equal(a.localPreviewUri, "file:///tmp/a.jpg");
  assert.equal(a.content_type, "image/jpeg");
  assert.equal(a.filename, "a.jpg");
  assert.equal(a.uploaded_at, 42);
  assert.equal(a.width, 400);
});

test("the draft viewer walks the strip's images and videos in strip order", () => {
  const strip = [img("a"), pdf("doc"), vid("b"), img("c")];
  const { items, index } = draftViewerItems(strip, "b");
  assert.deepEqual(
    items.map((i) => i.id),
    ["a", "b", "c"],
  );
  assert.equal(index, 1);
  assert.ok(items.every((i) => !i.object_key && i.localPreviewUri));
});

test("the draft viewer has nothing for an attachment that left the strip", () => {
  assert.deepEqual(draftViewerItems([img("a")], "gone").items, []);
});

test("the strip hands a copy to the viewer, not the live array", () => {
  const strip = [img("a"), vid("b")];
  setDraftForViewer(strip);
  strip.pop();
  assert.deepEqual(
    getDraftForViewer().map((p) => p.id),
    ["a", "b"],
  );
});

test("picker assets: a video the picker could not name stays a video", () => {
  const picked = pickedFromAssets(
    [
      { assetId: "x", uri: "file:///x", fileName: "IMG_1.HEIC", mimeType: "image/heic", width: 1, height: 2 },
      { uri: "file:///v", type: "video" },
      { uri: "file:///i" },
    ],
    7,
  );
  assert.deepEqual(picked[0], {
    id: "x",
    uri: "file:///x",
    name: "IMG_1.HEIC",
    mimeType: "image/heic",
    width: 1,
    height: 2,
  });
  assert.equal(picked[1].id, "7-1");
  assert.equal(picked[1].name, "video-7-1.mp4");
  assert.equal(picked[1].mimeType, "video/mp4");
  assert.equal(picked[2].name, "image-7-2.jpg");
  assert.equal(picked[2].mimeType, "image/jpeg");
});

test("both composers with attachments pick photos and videos", () => {
  for (const file of ["app/chat/[id].tsx", "app/vault.tsx"]) {
    const src = read(file);
    assert.match(src, /mediaTypes: \["images", "videos"\]/, file);
    assert.match(src, /pickedFromAssets\(/, file);
  }
});

test("the composer draws preview cards in a horizontal scroller, not name chips", () => {
  const src = read("components/chat/Composer.tsx");
  assert.match(src, /<ComposerAttachmentPreview/);
  assert.match(src, /testID="strip-attachments"\s+horizontal/);
  assert.doesNotMatch(src, /chip-attachment-/);
});

test("the card carries its ids and labels, and remove lives in the shared toolbar", () => {
  const src = read("components/chat/ComposerAttachmentPreview.tsx");
  assert.match(src, /testID=\{composerAttachmentTestID\(index\)\}/);
  assert.match(src, /testID: removeAttachmentTestID\(index\)/);
  assert.match(src, /<AttachmentToolbar/);
  assert.match(src, /composer\.removeAttachment/);
  assert.match(src, /attachment\.viewLabel/);
});

test("the toolbar and card have no borders (mobile design rule)", () => {
  for (const file of [
    "components/chat/AttachmentToolbar.tsx",
    "components/chat/ComposerAttachmentPreview.tsx",
  ]) {
    assert.doesNotMatch(read(file), /border(Width|Style|Color|Start|End|Left|Right)/, file);
  }
});

test("toolbar buttons reach 44pt and a toggle reports its state", () => {
  const src = read("components/chat/AttachmentToolbar.tsx");
  assert.match(src, /const SLOP = \(44 - BUTTON\) \/ 2;/);
  assert.match(src, /hitSlop=\{SLOP\}/);
  assert.match(src, /accessibilityState=\{action\.selected === undefined \? undefined : \{ checked: on \}\}/);
});
