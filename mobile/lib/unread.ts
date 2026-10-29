// Unread badges from the persisted read cursors (#1422), the pure half; the hook is hooks/queries/useUnread.ts.

export interface UnreadCount {
  conversation_id: string;
  count: number;
}

/** Whole-set replacement, not a merge: an absent conversation has nothing. The one on screen never badges. */
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
