// Pure rules for sensitive media (#1568).

import type { MessageAttachment } from "../types";

/** Null while uploading: placeholder ids repeat across messages, so those reveal in the tile alone. */
export function revealKey(attachment: MessageAttachment): string | null {
  return attachment.object_key ? attachment.id : null;
}

export function isCovered(attachment: MessageAttachment, revealed: boolean): boolean {
  return attachment.sensitive === true && !revealed;
}

/** A covered tile's tap only reveals; opening takes a second tap. */
export function tileTap(attachment: MessageAttachment, revealed: boolean): "reveal" | "open" {
  return isCovered(attachment, revealed) ? "reveal" : "open";
}
