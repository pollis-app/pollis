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
 * Converge this device's read positions with the rest of the account, once, at
 * startup (#844).
 *
 * Rust pulls the account's encrypted cursor blob from the DS, merges it into the
 * local cursors (a `max()`, so nothing rewinds), and pushes the merged set back.
 * The result is how many local cursors moved; when any did, the badge counts
 * derived from them are stale and get refetched.
 *
 * ONCE, not on a timer. There is no `refetchInterval` here and there must not be
 * one — `no-periodic-polling.test.ts` enforces that, and the ongoing direction is
 * event-driven anyway: a cursor that advances schedules its own debounced push
 * inside Rust. A device that misses an update picks it up at the next launch or
 * resync rather than by asking every N seconds.
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
        // Offline, or the PIN is not entered yet so the account identity key
        // that decrypts the blob is unavailable. Neither is an error the user
        // can act on, and neither loses anything: the badges stay on this
        // device's own cursors until the next launch or resync.
        console.warn("read-cursor sync skipped", e);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [userId, queryClient]);
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
