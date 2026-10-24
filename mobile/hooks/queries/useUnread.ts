// Unread counts from the persisted read cursors (#1422). Mirrors desktop's
// `frontend/src/hooks/queries/useUnread.ts` (#844).
//
// Mobile's badges used to be in-memory only, bumped by a live `new_message`
// wake-up while the app was foregrounded and connected. A message that arrived
// while the app was backgrounded or killed — exactly the push-notification case
// — was ingested on catch-up and never counted, so the tab badge, the DM row,
// the group dot and the channel count all stayed blank; a cold start wiped
// every count. The counts are now derived in Rust from a durable
// per-conversation cursor against the messages ingest has stored.
//
// `appStore.unreadCounts` stays the projection every badge renders from. This
// module is the one place that writes the persisted answer into it, in an
// effect rather than inside the `queryFn` (a background refetch must not
// mutate observable state out of order with render, same as desktop). The live
// increment in `hooks/useInboxRealtime.ts` remains the instant path; each
// refetch then replaces the set with the truth.
//
// Refetched — never on a timer — when the device lands in the signed-in app
// (sign-in, unlock, cold start), on every return to the foreground, and after
// every ingest (`useIngestConversation`), which covers push receipt and the
// resume / reconnect catch-ups.

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

export type { UnreadCount } from "../../lib/unread";

export const unreadQueryKeys = {
  all: ["unread"] as const,
  counts: (userId: string | null) => ["unread", "counts", userId] as const,
};

/**
 * Every conversation with something unread. Conversations with nothing unread
 * are absent rather than zero, so this is the complete set of badges to show.
 * Disabled (`userId` null) while signed out or locked: the counts live in the
 * encrypted local DB, which is closed then.
 */
export function useUnreadCounts(userId: string | null) {
  return useQuery({
    queryKey: unreadQueryKeys.counts(userId),
    queryFn: async (): Promise<UnreadCount[]> =>
      (await invoke<UnreadCount[]>("get_unread_counts", { userId })) ?? [],
    enabled: !!userId,
    staleTime: 1000 * 30,
    // Every answer is a new object, even one equal to the last: the store may
    // have moved in between (a live bump, an optimistic clear), and only a
    // changed `data` reference re-runs the hydrate that puts it right.
    structuralSharing: false,
  });
}

/** Re-read the persisted counts. Cheap: one local-DB query. */
export function refreshUnread(qc: QueryClient = sharedQueryClient): void {
  void qc.invalidateQueries({ queryKey: unreadQueryKeys.all });
}

/**
 * Mark a conversation read up to the newest message this device holds.
 *
 * Names the conversation, never a position — Rust resolves which message that
 * is, so the app cannot write a cursor past a message it does not have. The
 * badge clears at once (the user opened it; the outcome is not in doubt) and
 * the refetch afterwards confirms it. A cursor that moved is pushed to the
 * account's other devices by the core itself, debounced.
 *
 * A plain function rather than a hook, so the list rows, Search and
 * `selectConversation` can call it wherever they cleared the badge before.
 */
export function markConversationRead(conversationId: string): void {
  appStore.markRead(conversationId);
  const userId = appStore.currentUser?.id;
  if (!userId) {
    return;
  }
  void invoke<boolean>("mark_conversation_read", { conversationId, userId })
    .catch((e) => {
      // Locked mid-call or the DB not open yet: the badge stays cleared here
      // and the cursor catches up the next time the conversation is opened.
      console.warn("[unread] mark_conversation_read failed:", e);
    })
    .finally(() => refreshUnread());
}

/**
 * Converge this device's read positions with the account's other devices.
 *
 * Rust pulls the account's encrypted cursor blob from the DS, merges it into
 * the local cursors (a `max()`, so nothing rewinds) and pushes the merged set
 * back. Returns how many local cursors moved.
 */
async function syncReadCursors(userId: string): Promise<number> {
  try {
    return (await invoke<number>("sync_read_cursors", { userId })) ?? 0;
  } catch (e) {
    // Offline, or the DS refused. Nothing is lost: the badges stay on this
    // device's own cursors until the next unlock or foreground.
    console.warn("[unread] read-cursor sync skipped:", e);
    return 0;
  }
}

/**
 * The whole unread lifecycle, mounted once with the other signed-in services
 * (`app/_layout.tsx`).
 */
export function useUnreadSync() {
  const signedInId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  // Only past the PIN: before it (and while locked) the local DB is closed and
  // every read would fail.
  const userId = signedInId && isInSignedInApp(segments, isLocked) ? signedInId : null;
  const qc = useQueryClient();

  const { data } = useUnreadCounts(userId);
  useEffect(() => {
    if (data) {
      appStore.hydrateUnread(data);
    }
  }, [data]);

  // Landing in the app — sign-in, unlock, cold start. A lock clears the query
  // cache, so read the counts afresh, then converge with the other devices:
  // reading on one device clears the badge on the rest. Desktop syncs once at
  // launch; a phone is unlocked and resumed far more often than launched, so
  // both count as that moment here.
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
    return () => {
      cancelled = true;
    };
  }, [userId, qc]);

  // Back to the foreground: whatever arrived while away is already in the
  // local DB if a push woke the app to ingest it, and the inbox catch-up
  // ingests the rest (each ingest refetches again).
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
    });
    return () => {
      sub.remove();
    };
  }, [userId, qc]);
}
