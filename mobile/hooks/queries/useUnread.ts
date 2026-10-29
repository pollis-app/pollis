// Unread counts from the persisted read cursors (#1422, desktop's #844). Written into appStore in an effect, never in the queryFn.

import { useEffect } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useQuery, useQueryClient, type QueryClient } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { useSegments } from "expo-router";
import { invoke } from "../../lib/native";
import { queryClient as sharedQueryClient } from "../../lib/queryClient";
import { isInSignedInApp } from "../../lib/enrollmentPrompt";
import { resumedToForeground, type UnreadCount } from "../../lib/unread";
import { appStore } from "../../stores/appStore";
import { dismissConversationNotifications, reconcileReadNotifications } from "../../lib/push";

export type { UnreadCount } from "../../lib/unread";

export const unreadQueryKeys = {
  all: ["unread"] as const,
  counts: (userId: string | null) => ["unread", "counts", userId] as const,
};

/** Every conversation with something unread (absent, not zero). Disabled while signed out or locked. */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: async (): Promise<UnreadCount[]> =>
      (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [],
    enabled: !!userId,
    staleTime: 1000 * 30,
    // Only a changed `data` reference re-runs the hydrate that corrects the store.
    structuralSharing: false,
  });
}

export function refreshUnread(qc: QueryClient = sharedQueryClient): void {
  void qc.invalidateQueries({ queryKey: unreadQueryKeys.all });
}

/** Mark read up to the newest message this device holds (Rust resolves it), and clear this device's tray. */
export function markConversationRead(conversationId: string): void {
  appStore.markRead(conversationId);
  void dismissConversationNotifications(conversationId);
  const userId = appStore.currentUser?.id;
  if (!userId) {
    return;
  }
  void invoke<boolean>("mark_conversation_read", { conversationId, userId })
    .catch((e) => {
      // The cursor catches up the next time the conversation is opened.
      console.warn("[unread] mark_conversation_read failed:", e);
    })
    .finally(() => refreshUnread());
}

/** Converge read positions with the account's other devices (a `max()` merge). */
async function syncReadCursors(userId: string): Promise<number> {
  try {
    return (await invoke<number>("sync_read_cursors", { userId })) ?? 0;
  } catch (e) {
    // Nothing is lost: retried on the next unlock or foreground.
    console.warn("[unread] read-cursor sync skipped:", e);
    return 0;
  }
}

export function useUnreadSync() {
  const signedInId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  // Only past the PIN: the local DB is closed before it.
  const userId = signedInId && isInSignedInApp(segments, isLocked) ? signedInId : null;
  const qc = useQueryClient();

  const { data } = useUnreadCounts(userId);
  useEffect(() => {
    if (data) {
      appStore.hydrateUnread(data);
    }
  }, [data]);

  // Landing in the app: a lock clears the query cache, so read afresh, then converge.
  useEffect(() => {
    if (!userId) {
      return;
    }
    let cancelled = false;
    refreshUnread(qc);
    void syncReadCursors(userId).then((moved) => {
      if (!cancelled && moved > 0) {
        refreshUnread(qc);
      }
    });
    // #1508: clear what was read elsewhere while this device missed the silent push.
    void reconcileReadNotifications(userId);
    return () => {
      cancelled = true;
    };
  }, [userId, qc]);

  useEffect(() => {
    if (!userId) {
      return;
    }
    let previous: AppStateStatus = AppState.currentState;
    const sub = AppState.addEventListener("change", (next) => {
      const resumed = resumedToForeground(previous, next);
      previous = next;
      if (!resumed) {
        return;
      }
      refreshUnread(qc);
      void syncReadCursors(userId).then((moved) => {
        if (moved > 0) {
          refreshUnread(qc);
        }
      });
      void reconcileReadNotifications(userId);
    });
    return () => {
      sub.remove();
    };
  }, [userId, qc]);
}
