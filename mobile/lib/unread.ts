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

/**
 * One cross-device converge (#1628), with its effects injected: ask the core which conversations
 * were read elsewhere, then re-read the badges and clear those conversations' notifications.
 * Concurrent callers share one run plus at most one trailing run.
 */
export function createReadCursorConverge(deps: {
  refresh: () => Promise<string[] | null | undefined>;
  refreshUnread: () => void;
  dismiss: (conversationId: string) => Promise<void>;
  warn: (message: string, error: unknown) => void;
}): () => Promise<string[]> {
  let inFlight: Promise<string[]> | null = null;
  let again = false;
  return () => {
    if (inFlight) {
      again = true;
      return inFlight;
    }
    inFlight = (async () => {
      const moved: string[] = [];
      do {
        again = false;
        try {
          moved.push(...((await deps.refresh()) ?? []));
        } catch (e) {
          // Nothing is lost: retried on the next unlock, foreground, reconnect or nudge.
          deps.warn("[unread] read-cursor sync skipped:", e);
        }
      } while (again);
      if (moved.length > 0) {
        deps.refreshUnread();
        await Promise.all([...new Set(moved)].map((id) => deps.dismiss(id)));
      }
      return moved;
    })().finally(() => {
      inFlight = null;
    });
    return inFlight;
  };
}
