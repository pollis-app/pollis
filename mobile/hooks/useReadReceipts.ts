// Mark-read reporting for the chat screen's inverted FlatList (#892).
//
// Mirrors desktop's `frontend/src/hooks/useReadReceipts.ts` thresholds:
// a row counts as read only when ≥60% of it is visible for ~600ms while the
// app is foregrounded. On RN, `viewabilityConfig`'s
// `itemVisiblePercentThreshold` + `minimumViewTime` fold the visibility
// ratio and the dwell into one config; the desktop's document-focus gate
// maps to `AppState === "active"`.
//
// Ids are batched with a trailing debounce and sent once ever per
// conversation open (`reported` set) via `mark_messages_read`. The core
// drops own-messages and enforces the `send_read_receipts` preference at
// the emit chokepoint, but the caller still gates `enabled` on the
// preference so a disabled user does no work at all.

import { useEffect, useMemo, useRef } from "react";
import { AppState, type ViewToken } from "react-native";
import { invoke } from "../lib/native";

const VISIBLE_PERCENT = 60;
const DWELL_MS = 600;

type RowLike = {
  type: string;
  message?: { id: string; sender_id: string; pending?: boolean };
};

export function useReadReceipts(
  conversationId: string | null,
  userId: string | null,
  enabled: boolean,
) {
  const reportedRef = useRef<Set<string>>(new Set());
  const pendingRef = useRef<Set<string>>(new Set());
  const flushTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Live copies for the stable viewability callback.
  const stateRef = useRef({ conversationId, userId, enabled });
  stateRef.current = { conversationId, userId, enabled };

  // A new conversation starts clean — never carry another conversation's
  // reported set across.
  useEffect(() => {
    reportedRef.current = new Set();
    pendingRef.current = new Set();
    if (flushTimerRef.current) {
      clearTimeout(flushTimerRef.current);
      flushTimerRef.current = null;
    }
  }, [conversationId]);

  const flushRef = useRef(() => {});
  flushRef.current = () => {
    const { conversationId: convId, userId: uid, enabled: on } =
      stateRef.current;
    if (!on || !convId || !uid) {
      return;
    }
    if (AppState.currentState !== "active") {
      // Held, not dropped — the AppState listener below re-flushes.
      return;
    }
    const batch = [...pendingRef.current].filter(
      (id) => !reportedRef.current.has(id),
    );
    pendingRef.current.clear();
    if (batch.length === 0) {
      return;
    }
    // Mark reported BEFORE awaiting — an in-flight failure must not spin.
    for (const id of batch) {
      reportedRef.current.add(id);
    }
    void invoke("mark_messages_read", {
      conversationId: convId,
      userId: uid,
      messageIds: batch,
    }).catch((err) => {
      console.error("[receipts] mark_messages_read failed", err);
    });
  };

  const scheduleFlush = () => {
    if (flushTimerRef.current) {
      clearTimeout(flushTimerRef.current);
    }
    flushTimerRef.current = setTimeout(() => {
      flushTimerRef.current = null;
      flushRef.current();
    }, DWELL_MS);
  };

  const scheduleFlushRef = useRef(scheduleFlush);
  scheduleFlushRef.current = scheduleFlush;

  // Returning to the foreground flushes whatever is already on screen.
  useEffect(() => {
    const sub = AppState.addEventListener("change", (next) => {
      if (next === "active") {
        scheduleFlushRef.current();
      }
    });
    return () => {
      sub.remove();
      if (flushTimerRef.current) {
        clearTimeout(flushTimerRef.current);
      }
    };
  }, []);

  // `viewabilityConfigCallbackPairs` must be referentially stable for the
  // lifetime of the FlatList, so the pair delegates to refs.
  const viewabilityConfigCallbackPairs = useMemo(
    () => [
      {
        viewabilityConfig: {
          itemVisiblePercentThreshold: VISIBLE_PERCENT,
          minimumViewTime: DWELL_MS,
        },
        onViewableItemsChanged: ({
          viewableItems,
        }: {
          viewableItems: ViewToken[];
        }) => {
          const { userId: uid, enabled: on } = stateRef.current;
          if (!on || !uid) {
            return;
          }
          let changed = false;
          for (const token of viewableItems) {
            const item = token.item as RowLike | undefined;
            if (!token.isViewable || !item || item.type !== "msg") {
              continue;
            }
            const message = item.message;
            if (!message || message.pending || message.sender_id === uid) {
              continue;
            }
            if (
              reportedRef.current.has(message.id) ||
              pendingRef.current.has(message.id)
            ) {
              continue;
            }
            pendingRef.current.add(message.id);
            changed = true;
          }
          if (changed) {
            scheduleFlushRef.current();
          }
        },
      },
    ],
    [],
  );

  return { viewabilityConfigCallbackPairs };
}
