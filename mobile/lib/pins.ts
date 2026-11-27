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

/** What an optimistic pin is drawn from: the local row being pinned. */
export interface PinSource {
  content: string;
  sender_id: string;
  sender_username?: string | null;
  sender_display_name?: string | null;
  sent_at: string;
}

/**
 * The pinned list as it will read once a pin or unpin lands (#2141), so the tap shows at once.
 * A pin is drawn from the local row (the core snapshots the same row) and replaced by the real
 * snapshot when the list is re-read; pinning what is already pinned changes nothing.
 */
export function togglePinLocally(
  pins: readonly PinnedMessage[],
  change: {
    conversationId: string;
    messageId: string;
    isPinned: boolean;
    pinnedBy: string;
    source?: PinSource | null;
    now?: string;
  },
): PinnedMessage[] {
  if (change.isPinned) {
    return pins.filter((p) => p.message_id !== change.messageId);
  }
  if (!change.source || pins.some((p) => p.message_id === change.messageId)) {
    return [...pins];
  }
  const pin: PinnedMessage = {
    id: `pending-pin-${change.messageId}`,
    conversation_id: change.conversationId,
    message_id: change.messageId,
    pinned_by: change.pinnedBy,
    pinned_at: change.now ?? new Date().toISOString(),
    content: change.source.content,
    sender_id: change.source.sender_id,
    sender_username: change.source.sender_username ?? null,
    sender_display_name: change.source.sender_display_name ?? null,
    sent_at: change.source.sent_at,
    link_preview: null,
  };
  // Newest first, as the core lists them.
  return [pin, ...pins];
}
