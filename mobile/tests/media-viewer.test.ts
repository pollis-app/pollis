/*
 * The full-screen media viewer's rules (#1248), pinned without a renderer:
 * which attachments walk together, in what order, what the exported copy is
 * named (the photo library and AVPlayer type a file by its extension), and
 * that the route stays a pushed page rather than a modal.
 *
 *   node --test mobile/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  canSaveToLibrary,
  aliasAttachment,
  collectViewerItems,
  currentAttachmentId,
  exportFilename,
  formatClock,
  mediaKind,
} from "../lib/media/viewer.ts";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");

function att(id: string, content_type: string, filename = `${id}.bin`) {
  return {
    id,
    object_key: `k-${id}`,
    content_hash: `h-${id}`,
    filename,
    content_type,
    file_size: 10,
    uploaded_at: 0,
  };
}

test("kinds follow the MIME type, and only stills and video go to the library", () => {
  assert.equal(mediaKind("image/png"), "image");
  assert.equal(mediaKind("video/mp4"), "video");
  assert.equal(mediaKind("audio/mpeg"), "audio");
  assert.equal(mediaKind("application/pdf"), "file");
  assert.equal(canSaveToLibrary("image"), true);
  assert.equal(canSaveToLibrary("video"), true);
  assert.equal(canSaveToLibrary("audio"), false);
  assert.equal(canSaveToLibrary("file"), false);
});

test("an image walks the conversation's images and videos, oldest first", () => {
  // Newest-first, as flattenPages yields them.
  const messages = [
    { id: "m3", created_at: 3, attachments: [att("c", "image/png")] },
    { id: "m2", created_at: 2, attachments: [att("v", "video/mp4"), att("s", "audio/mpeg")] },
    { id: "m1", created_at: 1, attachments: [att("a", "image/jpeg"), att("f", "application/pdf")] },
  ];
  const { items, index } = collectViewerItems(messages, "v");
  assert.deepEqual(items.map((a) => a.id), ["a", "v", "c"]);
  assert.equal(index, 1);
});

test("audio and other files open alone", () => {
  const messages = [
    { id: "m1", created_at: 1, attachments: [att("a", "image/jpeg"), att("s", "audio/mpeg")] },
  ];
  assert.deepEqual(collectViewerItems(messages, "s").items.map((a) => a.id), ["s"]);
});

test("deleted messages drop out of the roll, and an unknown id yields nothing", () => {
  const messages = [
    { id: "m2", created_at: 2, attachments: [att("b", "image/png")], deleted_at: 5 },
    { id: "m1", created_at: 1, attachments: [att("a", "image/png")] },
  ];
  assert.deepEqual(collectViewerItems(messages, "a").items.map((a) => a.id), ["a"]);
  assert.deepEqual(collectViewerItems(messages, "zz").items, []);
});

test("exported copies keep the sender's name and gain an extension when missing", () => {
  assert.equal(exportFilename("holiday.JPG", "image/jpeg", "abc"), "holiday.JPG");
  assert.equal(exportFilename("clip", "video/quicktime", "abc"), "clip.mov");
  assert.equal(exportFilename("../../etc/passwd", "text/plain", "abc"), "_.._etc_passwd.txt");
  assert.equal(exportFilename("   ", "image/png", "0123456789abcdef"), "pollis-0123456789ab.png");
  assert.equal(exportFilename("notes", "application/x-unknown", "abc"), "notes");
  const long = `${"x".repeat(300)}.png`;
  const trimmed = exportFilename(long, "image/png", "abc");
  assert.ok(trimmed.length <= 120);
  assert.ok(trimmed.endsWith(".png"));
});

test("the audio clock reads m:ss and h:mm:ss", () => {
  assert.equal(formatClock(0), "0:00");
  assert.equal(formatClock(65.9), "1:05");
  assert.equal(formatClock(3729), "1:02:09");
  assert.equal(formatClock(Number.NaN), "0:00");
});

test("the viewer is a pushed page, not a modal", () => {
  const src = readFileSync(join(ROOT, "app/media.tsx"), "utf8");
  assert.doesNotMatch(src, /<Modal\b|presentation:\s*["']modal/);
  const layout = readFileSync(join(ROOT, "app/_layout.tsx"), "utf8");
  assert.match(layout, /<Stack\.Screen name="media" \/>/);
});

test("a viewer opened on a pending attachment follows it to the confirmed id", () => {
  assert.equal(currentAttachmentId("picked-1"), "picked-1");
  aliasAttachment("picked-1", "r2/key-1");
  assert.equal(currentAttachmentId("picked-1"), "r2/key-1");
  assert.equal(currentAttachmentId("r2/key-1"), "r2/key-1");
  // The confirmed message is what the cache holds after the send settles.
  const confirmed = [{ id: "m1", created_at: 1, attachments: [att("r2/key-1", "image/png")] }];
  const { items, index } = collectViewerItems(confirmed, currentAttachmentId("picked-1"));
  assert.deepEqual(items.map((a) => a.id), ["r2/key-1"]);
  assert.equal(index, 0);
});

test("the send settles its pending attachments' ids for the viewer, and the viewer reads them", () => {
  const send = readFileSync(join(ROOT, "hooks/queries/useMessages.ts"), "utf8");
  assert.match(send, /onSuccess:[\s\S]*?aliasAttachment\(att\.id, to\)[\s\S]*?setQueryData/);
  const viewer = readFileSync(join(ROOT, "app/media.tsx"), "utf8");
  assert.match(viewer, /collectViewerItems\(messages, currentAttachmentId\(attachmentId\)\)/);
  assert.match(viewer, /currentId = currentAttachmentId\(trackedId\)/);
});
