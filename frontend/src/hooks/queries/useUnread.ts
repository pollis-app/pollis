/**
 * Unread counts, derived in Rust from the persisted per-conversation read
 * cursor (#844). `useHydrateUnread` is the only writer of
 * `appStore.unreadCounts`, from an effect, never a `queryFn`
 * (`query-store-boundary.test.ts`).
 */
import { useEffect } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";

/** Mirrors `pollis_core::commands::messages::UnreadCount`. */
export interface UnreadCount {
  conversation_id: string;
  count: number;
}

export const unreadQueryKeys = {
  all: ["unread"] as const,
  counts: (userId: string | null) => ["unread", "counts", userId] as const,
};

/**
 * Every conversation with something unread. Conversations with nothing unread
 * are absent rather than zero, so this is the complete set of badges to show.
 */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: async (): Promise<UnreadCount[]> =>
      (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [],
    enabled: !!userId,
    // Ingest and the live dispatcher invalidate this; no periodic refetch.
    staleTime: 1000 * 30,
  });
}

/**
 * Copy the query's answer into the store the badges observe. In an effect:
 * inside the `queryFn` a background refetch would mutate observable state out
 * of order with render.
 */
export function useHydrateUnread(userId: string | null) {
  const { data } = useUnreadCounts(userId);
  useEffect(() => {
    if (data) {
      appStore.hydrateUnread(data);
    }
  }, [data]);
}

/**
 * Merge this device's read cursors with the account's (a `max()`, so nothing
 * rewinds) once at startup, never on a timer (#844). Rust returns how many
 * local cursors moved; any move refetches the badges.
 */
export function useSyncReadCursors(userId: string | null) {
  const queryClient = useQueryClient();
  useEffect(() => {
    if (!userId) {
      return;
    }
    let cancelled = false;
    void (async () => {
      try {
        const moved = await invoke<number>("sync_read_cursors", { userId });
        if (!cancelled && moved) {
          void queryClient.invalidateQueries({ queryKey: unreadQueryKeys.all });
        }
      } catch (e) {
        // Offline or pre-PIN (no key to decrypt the blob): badges stay on this
        // device's cursors until the next launch or resync.
        console.warn("read-cursor sync skipped", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [userId, queryClient]);
}

/**
 * Mark a conversation read up to the newest message this device holds. Names
 * the conversation, never a position, so the renderer cannot write a cursor to
 * a message that isn't the newest. The badge clears optimistically.
 */
export function useMarkConversationRead(userId: string | null) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (conversationId: string) => {
      if (!userId) {
        return false;
      }
      return await invoke<boolean>("mark_conversation_read", {
        conversationId,
        userId,
      });
    },
    onMutate: (conversationId: string) => {
      appStore.markRead(conversationId);
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: unreadQueryKeys.all });
    },
  });
}
