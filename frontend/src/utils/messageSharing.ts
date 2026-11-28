// Row identity across refetches of a conversation's live page (#2167).
//
// The page is the newest 50, so one arrival shifts every index, and React Query's default sharing
// (by index) hands every row a new object and re-renders it. Rows are matched by id instead, and each
// is deep-compared with `replaceEqualDeep`: any changed field (send state, edit, delete, decryption)
// gives the row a new object, so a memoised row always repaints a change. Never an id-only or shallow
// "same row" check: that would hide a failed send.

import { replaceEqualDeep } from "@tanstack/react-query";
import type { Message } from "../types/index.ts";

/** The shape of a conversation's cached live page. */
export interface MessagePageData {
  messages: Message[];
  nextCursor: { sent_at: string; id: string } | null;
}

function isPage(value: unknown): value is MessagePageData {
  return (
    typeof value === "object" &&
    value !== null &&
    Array.isArray((value as { messages?: unknown }).messages)
  );
}

/** `structuralSharing` for the live page: share rows by id, deep-compared; anything else falls back to the default. */
export function shareMessagesById(oldData: unknown, newData: unknown): unknown {
  if (!isPage(oldData) || !isPage(newData)) {
    return replaceEqualDeep(oldData, newData);
  }
  const prevById = new Map(oldData.messages.map((m) => [m.id, m]));
  let unchanged = oldData.messages.length === newData.messages.length;
  const messages = newData.messages.map((m, i) => {
    const before = prevById.get(m.id);
    const kept = before ? replaceEqualDeep(before, m) : m;
    if (kept !== oldData.messages[i]) {
      unchanged = false;
    }
    return kept;
  });
  const nextCursor = replaceEqualDeep(oldData.nextCursor, newData.nextCursor);
  if (unchanged && nextCursor === oldData.nextCursor) {
    return oldData;
  }
  return { ...newData, messages, nextCursor };
}

/**
 * Freeze a cached row and what hangs off it. Identity reuse is only safe while no one mutates a
 * cached row in place (a mutated old row deep-equals the new one, so the change would never paint);
 * in development and tests every cached row is frozen, so such a write throws instead.
 */
export function freezeMessage(message: Message): Message {
  if (Object.isFrozen(message)) {
    return message;
  }
  for (const attachment of message.attachments ?? []) {
    Object.freeze(attachment);
  }
  if (message.attachments) {
    Object.freeze(message.attachments);
  }
  if (message.link_preview) {
    if (message.link_preview.image) {
      Object.freeze(message.link_preview.image);
    }
    Object.freeze(message.link_preview);
  }
  return Object.freeze(message);
}

/** `shareMessagesById`, plus `freezeMessage` on every row in development builds. */
export function shareMessagePage(freeze: boolean) {
  return (oldData: unknown, newData: unknown): unknown => {
    const shared = shareMessagesById(oldData, newData);
    if (freeze && isPage(shared)) {
      shared.messages.forEach(freezeMessage);
    }
    return shared;
  };
}
