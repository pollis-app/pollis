/**
 * Unread counts, derived from the persisted read cursor (#844).
 *
 * Unread used to be a plain in-memory MobX field whose only writer was the
 * live-event dispatcher, which made it wrong in the two ways that matter: a
 * restart marked everything read, and anything that arrived while the app was
 * closed never badged at all. Both made users silently miss messages.
 *
 * The counts are now DERIVED in Rust from a durable per-conversation cursor
 * against the messages ingest has already stored, so this is remote-ish data
 * with a source of truth outside the renderer — a React Query hook, not store
 * state. `appStore.unreadCounts` remains as the UI projection the badges read;
 * `useHydrateUnread` is the one place that copies one into the other, in an
 * effect rather than inside a `queryFn` (see `query-store-boundary.test.ts` —
 * a background refetch writing observable state is the race that guards
 * against).
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
    // Ingest and the live dispatcher both invalidate this; there is no
    // periodic refetch (see `no-periodic-polling.test.ts`).
    staleTime: 1000 * 30,
  });
}

/**
 * Copy the query's answer into the store the badges observe.
 *
 * In an effect, deliberately: doing it inside the `queryFn` would let a
 * background refetch mutate observable state out of order with render.
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
 * Mark a conversation read up to the newest message this device holds.
 *
 * Names the conversation, never a position — Rust resolves which message that
 * is from the rows it already has, so the renderer cannot write a cursor
 * pointing at a message that does not exist or is not actually the newest.
 *
 * The badge is cleared optimistically because the outcome is not in doubt: the
 * user opened the conversation. The refetch that follows confirms it.
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
