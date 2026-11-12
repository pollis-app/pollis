// Where a video tile's still frame comes from (#1837), pure. Desktop's AttachmentDisplay follows
// the same order: the sender's `pv`; a pending send's own file; for a sender that had no poster
// (an older build) the file decoded here, fetched for that only while it is small; else the
// blurhash or the filled placeholder the tile always draws.

import type { MessageAttachment } from "../../types";

/** A video with no `pv` is fetched just to decode a frame only up to this size. As desktop. */
export const LEGACY_POSTER_MAX_BYTES = 25 * 1024 * 1024;

export type VideoPosterPlan = "pv" | "pending" | "legacy" | "none";

export function videoPosterPlan(
  att: Pick<MessageAttachment, "poster" | "object_key" | "localPreviewUri" | "file_size">,
  alreadyOnDisk: boolean,
): VideoPosterPlan {
  if (att.poster) {
    return "pv";
  }
  if (!att.object_key) {
    return att.localPreviewUri ? "pending" : "none";
  }
  return alreadyOnDisk || att.file_size <= LEGACY_POSTER_MAX_BYTES ? "legacy" : "none";
}
