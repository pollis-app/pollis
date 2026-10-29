// Unread counts from the persisted read cursors (#1422, desktop's #844).
// This module is the one writer of `appStore.unreadCounts`, from an effect
// rather than the `queryFn` so a background refetch cannot mutate observable
// state out of order with render. Refetched on landing, resume and after every
// ingest, never on a timer.

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

/**
 * Every conversation with something unread (nothing unread = absent). Pass a
 * null `userId` while signed out or locked: the local DB is closed then.
 */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: async (): Promise<UnreadCount[]> =>
      (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [],
    enabled: !!userId,
    staleTime: 1000 * 30,
    // The store may have moved since the last equal answer (a live bump, an
    // optimistic clear); only a new `data` reference re-runs the hydrate.
    structuralSharing: false,
  });
}

export function refreshUnread(qc: QueryClient = sharedQueryClient): void {
  void qc.invalidateQueries({ queryKey: unreadQueryKeys.all });
}

/**
 * Mark a conversation read up to the newest message this device holds. Names
 * the conversation, never a position, so the app cannot write a cursor past a
 * message it does not have. Also clears this device's tray for it, whether or
 * not the cursor moved (#1508).
 */
export function markConversationRead(conversationId: string): void {
  appStore.markRead(conversationId);
  void dismissConversationNotifications(conversationId);
  const userId = appStore.currentUser?.id;
  if (!userId) {
    return;
  }
  void invoke<boolean>("mark_conversation_read", { conversationId, userId })
    .catch((e) => {
      console.warn("[unread] mark_conversation_read failed:", e);
    })
    .finally(() => refreshUnread());
}

/** Merge read cursors with the account's other devices; returns how many moved. */
async function syncReadCursors(userId: string): Promise<number> {
  try {
    return (await invoke<number>("sync_read_cursors", { userId })) ?? 0;
  } catch (e) {
    console.warn("[unread] read-cursor sync skipped:", e);
    return 0;
  }
}

/** The whole unread lifecycle, mounted once in `app/_layout.tsx`. */
export function useUnreadSync() {
  const signedInId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  // Only past the PIN: before it (and while locked) the local DB is closed.
  const userId = signedInId && isInSignedInApp(segments, isLocked) ? signedInId : null;
  const qc = useQueryClient();

  const { data } = useUnreadCounts(userId);
  useEffect(() => {
    if (data) {
      appStore.hydrateUnread(data);
    }
  }, [data]);

  // Landing in the app (sign-in, unlock, cold start): read afresh, then
  // converge with the other devices.
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
    // Clear what was read elsewhere while this device missed the silent push (#1508).
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
