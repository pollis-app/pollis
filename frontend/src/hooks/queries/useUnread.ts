/** Unread counts derived in Rust from the persisted read cursor (#844); copied to the store in an effect. */
import { useEffect } from "react";
import { useMutation, useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
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

async function readUnreadCounts(userId: string | null): Promise<UnreadCount[]> {
  return (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [];
}

/** Conversations with something unread; the rest are absent. */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: () => readUnreadCounts(userId),
    enabled: !!userId,
    // Invalidated after every ingest, read and cursor sync (#1628); no periodic refetch.
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

let inFlight: Promise<string[]> | null = null;
let again = false;

/**
 * Converge read cursors with the account's other devices (pull, merge, push only if ahead) and
 * show the result (#1628): fresh unread counts, and no alert for a conversation now read.
 * Concurrent callers share one run plus at most one trailing run. Resolves to the conversations
 * whose cursor moved here.
 */
export function refreshReadCursors(queryClient: QueryClient, userId: string): Promise<string[]> {
  if (inFlight) {
    again = true;
    return inFlight;
  }
  inFlight = (async () => {
    const moved: string[] = [];
    do {
      again = false;
      try {
        moved.push(...((await invoke<string[]>("refresh_read_cursors", { userId })) ?? []));
      } catch (e) {
        // Offline or locked; the next focus, reconnect or nudge retries.
        console.warn("read-cursor sync skipped", e);
      }
    } while (again);
    if (moved.length > 0) {
      const counts = await queryClient.fetchQuery({
        queryKey: unreadQueryKeys.counts(userId),
        queryFn: () => readUnreadCounts(userId),
        staleTime: 0,
      });
      appStore.hydrateUnread(counts);
      appStore.clearAlertIfRead(moved);
    }
    return moved;
  })().finally(() => {
    inFlight = null;
  });
  return inFlight;
}

/** Learn reads made on the account's other devices: at startup and on every window focus (#1628). */
export function useSyncReadCursors(userId: string | null) {
  const queryClient = useQueryClient();
  useEffect(() => {
    if (!userId) {
      return;
    }
    void refreshReadCursors(queryClient, userId);
    const onFocus = () => {
      void refreshReadCursors(queryClient, userId);
    };
    window.addEventListener("focus", onFocus);
    return () => {
      window.removeEventListener("focus", onFocus);
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
