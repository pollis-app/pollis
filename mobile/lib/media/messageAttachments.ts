// How a message row lays out its attachments (#1970, desktop's MessageItem): pictures and videos
// first, then audio and files as a list. Two or more pictures and videos form a 2-up grid of
// equal squares; a lone one keeps its own shape. No react-native imports.

import type { MessageAttachment } from "../../types";
import { isRollMedia, mediaKind } from "./viewer.ts";

export interface MessageAttachmentLayout {
  /** Pictures and videos, in message order. */
  media: MessageAttachment[];
  /** Two or more: `media` in rows of two (the last may hold one). */
  gridRows: MessageAttachment[][] | null;
  /** Audio and other files, in message order. */
  files: MessageAttachment[];
}

const GRID_COLUMNS = 2;

export function messageAttachmentLayout(
  attachments: readonly MessageAttachment[],
): MessageAttachmentLayout {
  const media = attachments.filter((a) => isRollMedia(mediaKind(a.content_type)));
  const files = attachments.filter((a) => !isRollMedia(mediaKind(a.content_type)));
  let gridRows: MessageAttachment[][] | null = null;
  if (media.length > 1) {
    gridRows = [];
    for (let i = 0; i < media.length; i += GRID_COLUMNS) {
      gridRows.push(media.slice(i, i + GRID_COLUMNS));
    }
  }
  return { media, gridRows, files };
}
