// What a pinned-list row shows (#1315): a pin is a SNAPSHOT, parsed like a timeline row. Pure.

import { linkPreviewFromWire, parseContent } from "./messageContent.ts";
import type { LinkPreviewWire, MessageLinkPreview } from "../types";

export interface PinnedMessage {
  id: string;
  conversation_id: string;
  message_id: string;
  pinned_by: string;
  pinned_at: string;
  content: string;
  sender_id: string;
  sender_username?: string | null;
  /** #2085: the sender's display name when this device knows one. */
  sender_display_name?: string | null;
  sent_at: string;
  /** #2127: the sender's link preview, re-checked against `content` in the core. */
  link_preview?: LinkPreviewWire | null;
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

/** The pin's link preview, its thumbnail loading as an image of the pin's conversation (#2127). */
export function pinLinkPreview(
  pin: Pick<PinnedMessage, "conversation_id" | "link_preview">,
): MessageLinkPreview | null {
  return linkPreviewFromWire(pin.link_preview, { conversationId: pin.conversation_id });
}

export function pinnedIds(pins: readonly Pick<PinnedMessage, "message_id">[]): Set<string> {
  return new Set(pins.map((p) => p.message_id));
}
