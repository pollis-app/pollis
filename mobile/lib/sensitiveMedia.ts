// Pure rules for sensitive media (#1568). No react-native imports, so
// `tests/sensitive-media.test.ts` can pin them under plain node.

import type { MessageAttachment } from "../types";

/**
 * The id a reveal is remembered under, or null for an attachment still
 * uploading — its placeholder id (`uploading-<i>`) repeats across messages,
 * so a shared reveal would uncover the same position in every pending
 * message. Those keep their reveal in the tile alone.
 */
export function revealKey(attachment: MessageAttachment): string | null {
  return attachment.object_key ? attachment.id : null;
}

/** Whether the attachment is covered right now. */
export function isCovered(attachment: MessageAttachment, revealed: boolean): boolean {
  return attachment.sensitive === true && !revealed;
}

/**
 * What a tap on a tile does. A covered tile's tap reveals it and does nothing
 * else — so the tap that uncovers a video never opens the viewer or starts
 * playback; opening it takes a second, deliberate tap.
 */
export function tileTap(attachment: MessageAttachment, revealed: boolean): "reveal" | "open" {
  return isCovered(attachment, revealed) ? "reveal" : "open";
}
