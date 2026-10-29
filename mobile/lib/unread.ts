// Unread badges from the persisted read cursors (#1422), the pure half of
// `hooks/queries/useUnread.ts`. Counts come from core's `get_unread_counts`,
// as on desktop (#844).

/** Mirrors `pollis_core::commands::messages::UnreadCount`. */
export interface UnreadCount {
  conversation_id: string;
  count: number;
}

/**
 * Whole-set replacement, not a merge: the core returns every conversation with
 * something unread, so merging would strand a badge another device cleared. The
 * conversation on screen never badges.
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

export function resumedToForeground(previous: string, next: string): boolean {
  return previous !== "active" && next === "active";
}
