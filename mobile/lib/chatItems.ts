// The chat's inverted-list items (pure): day separators, safety-number notices and message rows,
// newest first. Thread replies stay out of the main timeline (#837).

import type { Message } from "../hooks/queries/useMessages";
import type { KeyChangeNotice } from "../hooks/queries/useSafety";
import { collapseUndecryptable } from "./collapseUndecryptable.ts";
import { placeKeyChangeNotices } from "./keyChangeTimeline.ts";

export type ChatListItem =
  | { type: "sep"; key: string; label: string }
  | { type: "msg"; key: string; message: Message; continued: boolean }
  | { type: "keyChange"; key: string; notice: KeyChangeNotice; at: number };

type MessageItem = Extract<ChatListItem, { type: "msg" }>;

/** Discord-style grouping window. */
export const GROUP_WINDOW_MS = 5 * 60 * 1000;

// One item object per (message object, grouping), so a rebuild after an arrival hands every
// unchanged row the item it already had (#2168). Keyed by the message object itself: a changed
// row is a new object (hooks/queries/messageRows.ts), so it can never pick up a stale item.
const messageItems = new WeakMap<Message, { continued?: MessageItem; alone?: MessageItem }>();

function messageItem(m: Message, continued: boolean): MessageItem {
  let entry = messageItems.get(m);
  if (!entry) {
    entry = {};
    messageItems.set(m, entry);
  }
  const slot = continued ? "continued" : "alone";
  let item = entry[slot];
  if (!item) {
    // Keyed by the core's id (#2141): a stub and the stored row it becomes are one element,
    // so the row neither remounts nor re-measures when its send settles.
    item = { type: "msg", key: m.outboxId ?? m.id, message: m, continued };
    entry[slot] = item;
  }
  return item;
}

export interface DayLabels {
  dayKey: (ts: number) => string;
  dayLabel: (ts: number) => string;
}

/** `messages` newest first, as the pages hold them; the items newest first, as the inverted list draws them. */
export function buildChatItems(
  messages: readonly Message[],
  notices: readonly KeyChangeNotice[],
  hasOlder: boolean,
  labels: DayLabels,
): ChatListItem[] {
  const chrono = collapseUndecryptable(
    [...messages].reverse().filter((m) => !m.thread_id || m.thread_id === m.id),
  );
  const out: ChatListItem[] = [];
  let lastKey = "";
  let prev: Message | null = null;
  // Safety-number changes (#1605) at the time they were detected; a system line breaks a group.
  const placed = placeKeyChangeNotices(
    notices,
    chrono.length ? chrono[0].created_at : null,
    chrono.length ? chrono[chrono.length - 1].created_at : null,
    hasOlder,
    false,
  );
  let next = 0;
  const emitNotice = (n: (typeof placed)[number]) => {
    const k = labels.dayKey(n.at);
    if (k !== lastKey) {
      out.push({ type: "sep", key: `sep-${k}`, label: labels.dayLabel(n.at) });
      lastKey = k;
    }
    out.push({
      type: "keyChange",
      key: `key-change-${n.notice.id}`,
      notice: n.notice,
      at: n.at,
    });
    prev = null;
  };
  for (const m of chrono) {
    while (next < placed.length && placed[next].at < m.created_at) {
      emitNotice(placed[next]);
      next += 1;
    }
    const k = labels.dayKey(m.created_at);
    if (k !== lastKey) {
      out.push({ type: "sep", key: `sep-${k}`, label: labels.dayLabel(m.created_at) });
      lastKey = k;
      prev = null;
    }
    // Discord-style grouping; a reply always starts its own group.
    const continued =
      prev !== null &&
      !m.reply_to_id &&
      prev.sender_id === m.sender_id &&
      m.created_at - prev.created_at < GROUP_WINDOW_MS;
    out.push(messageItem(m, continued));
    prev = m;
  }
  while (next < placed.length) {
    emitNotice(placed[next]);
    next += 1;
  }
  out.reverse();
  return out;
}
