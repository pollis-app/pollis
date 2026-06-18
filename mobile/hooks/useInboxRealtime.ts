// App-level foreground realtime for the signed-in user's personal inbox room
// (`inbox-${userId}`). Desktop publishes per-user events there — a new DM
// being created, being added to / removed from a group — so holding this
// connection while signed in keeps the groups and DM lists live without a
// manual refresh, the same way `useConversationRealtime` keeps an open chat
// live.
//
// Mounted once under the providers (see `app/_layout.tsx`). Entirely
// best-effort: if realtime is unavailable (no LiveKit URL, no token command,
// connect error) `connectRealtime` returns null and this is a clean no-op —
// the lists still refresh on their normal query lifecycle / screen focus.

import { useEffect, useRef } from "react";
import type { Room } from "livekit-client";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import { connectRealtime, disconnectRealtime } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import { dmQueryKeys } from "./queries/useDMChannels";
import { groupQueryKeys } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";

export function useInboxRealtime() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();

  // Read the live client through a ref so the (stable) event handler never
  // forces a reconnect.
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;

  useEffect(() => {
    if (!userId) {
      return;
    }

    let cancelled = false;
    let room: Room | null = null;

    const handleEvent = (event: RealtimeEvent) => {
      const qc = qcRef.current;
      switch (event.type) {
        case "dm_created":
          // A new DM (started by a peer) — refresh the DM list + requests.
          void qc.invalidateQueries({ queryKey: dmQueryKeys.all });
          break;
        case "membership_changed":
          // Added to / removed from a group, or a join request resolved —
          // refresh the group list and pending invites.
          void qc.invalidateQueries({ queryKey: groupQueryKeys.all });
          void qc.invalidateQueries({
            queryKey: groupInviteQueryKeys.pending(userId),
          });
          break;
        default:
          // Other event types are delivered on conversation rooms, not the
          // inbox; ignored here.
          break;
      }
    };

    connectRealtime(`inbox-${userId}`, handleEvent)
      .then((connected) => {
        if (cancelled) {
          // Signed out (or user changed) before connect resolved.
          disconnectRealtime(connected);
          return;
        }
        room = connected;
      })
      .catch((e) => {
        console.warn("[realtime] inbox connect error:", e);
      });

    return () => {
      cancelled = true;
      disconnectRealtime(room);
      room = null;
    };
  }, [userId, queryClient]);
}
