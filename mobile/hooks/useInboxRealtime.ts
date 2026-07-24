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
import { invoke } from "../lib/native";
import { connectRealtime, disconnectRealtime } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import { dmQueryKeys } from "./queries/useDMChannels";
import { groupQueryKeys } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";
import { messageQueryKeys, useIngestConversation } from "./queries/useMessages";

export function useInboxRealtime() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();
  const ingest = useIngestConversation();

  // Read the live client through a ref so the (stable) event handler never
  // forces a reconnect.
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

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
        case "all_mention": {
          // An @all in a group. Arrives on the per-user inbox room so it
          // reaches members even when they're not in the group's LiveKit
          // room. Ingest the referenced channel so the message lands, then
          // refresh that channel's message cache. (No foreground OS ping —
          // mobile/lib/push exposes no ready local-notification helper, and
          // this task doesn't build new notification infra.)
          void ingestRef.current(event.channel_id, "channel");
          void qc.invalidateQueries({
            queryKey: messageQueryKeys.conversation(event.channel_id, "channel"),
          });
          break;
        }
        case "device_revoked": {
          // One of this user's devices was revoked. The inbox is per-user, so
          // this reaches every device; the payload is ADVISORY and spoofable,
          // so we do NOT trust its device_id. Authoritatively confirm with the
          // backend whether THIS device is still registered and only then sign
          // out. The handler is sync, so chain rather than await. Any error
          // (offline / transient) is treated as "still registered" — we never
          // sign out on a blip.
          if (!userId) {
            break;
          }
          void invoke<boolean>("is_current_device_registered", { userId })
            .then((stillRegistered) => {
              if (stillRegistered) {
                return;
              }
              console.warn("[realtime] this device was revoked — signing out");
              return invoke("logout", { deleteData: false })
                .catch((e) => console.warn("[realtime] logout failed:", e))
                .then(() => {
                  appStore.logout();
                });
            })
            .catch((e) => {
              console.warn("[realtime] device_revoked check failed (ignored):", e);
            });
          break;
        }
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
