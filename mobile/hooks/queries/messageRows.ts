// A conversation's rows as the chat list holds them (pure): the wire row decoded, and a re-read
// that keeps every unchanged row's object (#2168), so a memoised row only renders when its own
// message changed. Desktop's twin is `shareMessagesById` (frontend useMessages.ts, #2167).

import { replaceEqualDeep } from "@tanstack/react-query";
import type { DecodedPage, Message, MessagesData, RawChannelMessage } from "./useMessages";
import { linkPreviewFromWire, parseContent } from "../../lib/messageContent.ts";
import { unsentFlags } from "./pendingMessages.ts";

/** One stored row as the list draws it. Every value is a function of the row, nothing of the read. */
export function transformMessage(raw: RawChannelMessage): Message {
  const parsed = parseContent(raw.content, { conversationId: raw.conversation_id });
  return {
    id: raw.id,
    conversation_id: raw.conversation_id,
    sender_id: raw.sender_id,
    sender_username: raw.sender_username,
    sender_display_name: raw.sender_display_name ?? null,
    content: parsed.text,
    attachments: parsed.attachments,
    link_preview: linkPreviewFromWire(raw.link_preview, { conversationId: raw.conversation_id }),
    reply_to_id: raw.reply_to_id ?? null,
    thread_id: raw.thread_id ?? null,
    created_at: new Date(raw.sent_at).getTime(),
    edited_at: raw.edited_at ? new Date(raw.edited_at).getTime() : undefined,
    deleted_at: raw.deleted_at ? new Date(raw.deleted_at).getTime() : undefined,
    undecryptable: raw.undecryptable === true ? true : undefined,
    edit_unsent:
      raw.edit_state === "failed" || raw.edit_state === "rejected" ? true : undefined,
    ...unsentFlags(raw.id, raw.send_state),
  };
}

/** Flatten newest-first pages, dropping duplicates where refetched page boundaries overlap. */
export function flattenRows(data: { pages: DecodedPage[] } | undefined): Message[] {
  if (!data) {
    return [];
  }
  const seen = new Set<string>();
  const out: Message[] = [];
  for (const page of data.pages) {
    for (const m of page.messages) {
      if (seen.has(m.id)) {
        continue;
      }
      seen.add(m.id);
      out.push(m);
    }
  }
  return out;
}

function isPages(data: unknown): data is MessagesData {
  const d = data as MessagesData | undefined;
  return !!d && Array.isArray(d.pages) && d.pages.every((p) => !!p && Array.isArray(p.messages));
}

/** The cached row a re-read row may keep: one that deep-equals it, else a new object sharing what did. */
function shareRow(candidates: readonly Message[] | undefined, next: Message): Message {
  if (!candidates) {
    return next;
  }
  for (const c of candidates) {
    if (replaceEqualDeep(c, next) === c) {
      return c;
    }
  }
  return replaceEqualDeep(candidates[0], next);
}

/**
 * `structuralSharing` for a conversation's infinite query (#2168). React Query's default shares by
 * array index, and every page is cut by cursor from the newest, so one arrival shifts every row
 * of every page and each gets a new object. This matches rows by id across ALL cached pages (a
 * row can slide from one page to the next), then deep-compares with `replaceEqualDeep`: a row
 * keeps its old object only when every field is equal, so a changed send state, edit, delete,
 * decryption or reaction-free content change always reaches the row as a new object. Never an
 * id-only or shallow "same row" check: that would hide exactly those changes. Anything that is
 * not a page set falls back to the default sharing.
 */
export function shareMessagePagesById(oldData: unknown, newData: unknown): unknown {
  if (!isPages(oldData) || !isPages(newData)) {
    return replaceEqualDeep(oldData, newData);
  }
  const prev = oldData;
  const next = newData;
  const byId = new Map<string, Message[]>();
  for (const page of prev.pages) {
    for (const m of page.messages) {
      const list = byId.get(m.id);
      if (list) {
        list.push(m);
      } else {
        byId.set(m.id, [m]);
      }
    }
  }
  let unchanged = prev.pages.length === next.pages.length;
  const pages = next.pages.map((page, i): DecodedPage => {
    const before = prev.pages[i];
    const messages = page.messages.map((m) => shareRow(byId.get(m.id), m));
    const nextCursor = replaceEqualDeep(before?.nextCursor ?? null, page.nextCursor);
    const same =
      !!before &&
      before.messages.length === messages.length &&
      messages.every((m, j) => m === before.messages[j]) &&
      nextCursor === before.nextCursor &&
      Object.keys(page).length === Object.keys(before).length;
    if (same) {
      return before;
    }
    unchanged = false;
    return { ...page, messages, nextCursor };
  });
  const pageParams = replaceEqualDeep(prev.pageParams, next.pageParams);
  if (unchanged && pageParams === prev.pageParams) {
    return prev;
  }
  return { ...next, pages, pageParams };
}
