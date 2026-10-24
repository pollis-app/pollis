// Unread badges from the persisted read cursors (#1422) — the pure half.
//
// Mobile's counts used to live only in memory, bumped by a live `new_message`
// wake-up. A message that arrived while the app was backgrounded or killed (the
// push-notification case) was ingested on catch-up but never counted, and a
// cold start wiped every badge. The counts now come from pollis-core
// (`get_unread_counts`), derived from a durable per-conversation cursor against
// the messages ingest has stored, exactly as desktop does (#844). The hook that
// drives it is `hooks/queries/useUnread.ts`; the rules worth pinning live here,
// free of React Native, so `tests/unread.test.ts` can run them under node.

/** Mirrors `pollis_core::commands::messages::UnreadCount`. */
export interface UnreadCount {
  conversation_id: string;
  count: number;
}

/**
 * The badge set the store shows, from the core's answer.
 *
 * Whole-set replacement, not a merge: the core returns every conversation with
 * something unread, so one absent from the answer has nothing — merging would
 * strand a badge another device already cleared. The conversation on screen
 * never badges, even if the answer was read before its cursor advanced.
 */
export function unreadBadges(
  counts: ReadonlyArray<UnreadCount> | null | undefined,
  openConversationId: string | null,
): Record<string, number> {
  const next: Record<string, number> = {};
  for (const c of counts ?? []) {
    if (c.count <= 0 || c.conversation_id === openConversationId) {
      continue;
    }
    next[c.conversation_id] = c.count;
  }
  return next;
}

/**
 * Whether an AppState change is a return to the foreground — the moment a
 * phone catches up on whatever arrived while it was away.
 */
export function resumedToForeground(previous: string, next: string): boolean {
  return previous !== "active" && next === "active";
}
