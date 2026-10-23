// Pinned messages (#99, mobile #1315): what a row of the pinned list shows.
// Pure, so the node tests pin it.
//
// A pin is a SNAPSHOT of the message taken at pin time, sealed in Rust
// (pollis-core/src/commands/pinned_messages.rs). Its `content` is the same
// string shape the message table caches, so it goes through the same parser
// as a timeline row; an edit or delete after the pin does not change it, the
// same as desktop's PinnedPanel.

import { parseContent } from "./messageContent.ts";

/** Mirrors the Rust `PinnedMessage` (snake_case, as serde serializes it). */
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
  // The caption or message text, untrimmed (the row clamps its lines).
  text: string;
  // Attached files' names, in order; the row lists them under the text.
  filenames: string[];
};

export function pinPreview(content: string): PinPreview {
  const parsed = parseContent(content, null);
  return {
    text: parsed.text,
    filenames: parsed.attachments.map((a) => a.filename),
  };
}

/** The pinned message ids, for the message sheet's Pin / Unpin label. */
export function pinnedIds(pins: readonly Pick<PinnedMessage, "message_id">[]): Set<string> {
  return new Set(pins.map((p) => p.message_id));
}
