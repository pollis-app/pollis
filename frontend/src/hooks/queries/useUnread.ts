/** Unread counts derived in Rust from the persisted read cursor (#844); copied to the store in an effect. */
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

/** Conversations with something unread; the rest are absent. */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: async (): Promise<UnreadCount[]> =>
      (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [],
    enabled: !!userId,
    // Invalidated by ingest and the dispatcher; no periodic refetch.
    staleTime: 1000 * 30,
  });
}

/** Copy the counts into the store, in an effect rather than the queryFn. */
export function useHydrateUnread(userId: string | null) {
  const { data } = useUnreadCounts(userId);
  useEffect(() => {
    if (data) {
      appStore.hydrateUnread(data);
    }
  }, [data]);
}

/** Merge read cursors with the account once at startup (#844); never on a timer. */
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
        // Offline or PIN not entered; local cursors stand until the next launch.
        console.warn("read-cursor sync skipped", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [userId, queryClient]);
}

/** Mark a conversation read to its newest local message; Rust picks the position. */
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
