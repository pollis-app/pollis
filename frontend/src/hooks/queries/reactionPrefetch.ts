/* Reactions for a whole page in ONE `get_reactions_for_messages` read, seeded into each row's cache (#2115). */

import type { QueryClient } from "@tanstack/react-query";
import type { Reaction } from "../../types";

/** `invoke` from the bridge, injected so the cache rules are testable without Tauri. */
export type ReactionInvoke = <T>(command: string, args: Record<string, unknown>) => Promise<T>;

export const REACTIONS_STALE_MS = 15_000;

/** How long a freshly opened conversation waits for its reactions before painting without them. */
export const REACTIONS_REVEAL_WAIT_MS = 150;

export const reactionQueryKeys = {
  message: (messageId: string) => ["reactions", messageId] as const,
};

/**
 * One row's query. A cold cache still reads the one message; a warm one does not refetch
 * on mount (rows remount as the list virtualizes), since the page batch refreshes stale ids.
 */
export function reactionQueryOptions(messageId: string, invoke: ReactionInvoke) {
  return {
    queryKey: reactionQueryKeys.message(messageId),
    queryFn: (): Promise<Reaction[]> => invoke<Reaction[]>("get_reactions", { messageId }),
    staleTime: REACTIONS_STALE_MS,
    refetchOnMount: false,
  } as const;
}

/** The ids that draw a pill row: a deleted message draws none (MessageItem). */
export function reactableIds(messages: ReadonlyArray<{ id: string; deleted_at?: string | null }>): string[] {
  return messages.filter((m) => !m.deleted_at).map((m) => m.id);
}

/** True when `messageId` has no cached reactions, or stale ones, and none are being read. */
function needsRead(queryClient: QueryClient, messageId: string, now: number): boolean {
  const state = queryClient.getQueryState(reactionQueryKeys.message(messageId));
  if (!state) {
    return true;
  }
  if (state.fetchStatus === "fetching") {
    return false;
  }
  if (state.data === undefined || state.isInvalidated) {
    return true;
  }
  return now - state.dataUpdatedAt > REACTIONS_STALE_MS;
}

/**
 * Read every listed message that needs it in one call, as each row's own in-flight fetch,
 * so a row that mounts meanwhile waits on the batch instead of reading alone.
 * Resolves once every entry has landed; never rejects (a row falls back to its own read).
 */
export function prefetchReactions(
  queryClient: QueryClient,
  messageIds: readonly string[],
  invoke: ReactionInvoke,
): Promise<void> {
  const now = Date.now();
  const ids = [...new Set(messageIds)].filter((id) => needsRead(queryClient, id, now));
  if (ids.length === 0) {
    return Promise.resolve();
  }
  const batch = invoke<Record<string, Reaction[]>>("get_reactions_for_messages", {
    messageIds: ids,
  });
  return Promise.all(
    ids.map((id) =>
      queryClient
        .fetchQuery({
          queryKey: reactionQueryKeys.message(id),
          queryFn: async () => (await batch)[id] ?? [],
          staleTime: 0,
        })
        .catch(() => undefined),
    ),
  ).then(() => undefined);
}

/** Resolves when `work` does or after `ms`, whichever is first. */
export function settleWithin(work: Promise<void>, ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    work.then(() => {
      clearTimeout(timer);
      resolve();
    });
  });
}
