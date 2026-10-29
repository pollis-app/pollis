// What a pinned-list row shows (#1315): a pin is a SNAPSHOT, parsed like a timeline row. Pure.

import { parseContent } from "./messageContent.ts";

export interface PinnedMessage {
  id: string;
  conversation_id: string;
  message_id: string;
  pinned_by: string;
  pinned_at: string;
  content: string;
  sender_id: string;
  sender_username?: string | null;
  sent_at: string;
}

export type PinPreview = {
  // Untrimmed; the row clamps its lines.
  text: string;
  filenames: string[];
};

export function pinPreview(content: string): PinPreview {
  const parsed = parseContent(content, null);
  return {
    text: parsed.text,
    filenames: parsed.attachments.map((a) => a.filename),
  };
}

export function pinnedIds(pins: readonly Pick<PinnedMessage, "message_id">[]): Set<string> {
  return new Set(pins.map((p) => p.message_id));
}
