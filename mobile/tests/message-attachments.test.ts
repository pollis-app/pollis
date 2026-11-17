/* A message row's attachment layout (#1970): a 2-up grid for several pictures, files after. */

import test from "node:test";
import assert from "node:assert/strict";

import { messageAttachmentLayout } from "../lib/media/messageAttachments.ts";
import type { MessageAttachment } from "../types/index.ts";

function att(id: string, content_type: string): MessageAttachment {
  return { id, content_type, filename: `${id}.bin`, file_size: 1 } as MessageAttachment;
}

test("a lone picture keeps its own shape (no grid)", () => {
  const layout = messageAttachmentLayout([att("a", "image/jpeg")]);
  assert.deepEqual(layout.media.map((a) => a.id), ["a"]);
  assert.equal(layout.gridRows, null);
  assert.deepEqual(layout.files, []);
});

test("three pictures make two rows: a pair, then one", () => {
  const layout = messageAttachmentLayout([
    att("a", "image/jpeg"),
    att("b", "image/png"),
    att("c", "video/mp4"),
  ]);
  assert.deepEqual(
    layout.gridRows?.map((row) => row.map((a) => a.id)),
    [["a", "b"], ["c"]],
  );
});

test("files and audio follow the media, in message order, never in the grid", () => {
  const layout = messageAttachmentLayout([
    att("doc", "application/pdf"),
    att("a", "image/jpeg"),
    att("song", "audio/mpeg"),
    att("b", "image/jpeg"),
  ]);
  assert.deepEqual(layout.media.map((a) => a.id), ["a", "b"]);
  assert.deepEqual(layout.gridRows?.map((row) => row.map((a) => a.id)), [["a", "b"]]);
  assert.deepEqual(layout.files.map((a) => a.id), ["doc", "song"]);
});

test("no attachments, no layout", () => {
  const layout = messageAttachmentLayout([]);
  assert.deepEqual(layout, { media: [], gridRows: null, files: [] });
});
