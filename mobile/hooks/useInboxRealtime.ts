// App-level foreground realtime for the signed-in user. Holds a data-only
// LiveKit subscription to:
//
//   - the personal inbox room (`inbox-${userId}`) — per-user events: a new DM
//     being created, group membership changes, @all/@username mentions,
//     device revocation;
//   - every group room (named by group id) and DM room (named by
//     conversation id) — `new_message` / `edited_message` / `deleted_message`
//     wake-ups, which is how desktop drives unread badges and live previews
//     (the backend publishes those to the conversation's room, never to the
//     inbox — see `pollis-core/src/commands/messages/send.rs`).
//
// Room connections are ref-counted in `lib/realtime/client`, so the open
// chat's `useConversationRealtime` sharing a room with this hook reuses one
// LiveKit connection instead of evicting it (same device identity).
//
// Mounted once under the providers (see `app/_layout.tsx`). Entirely
// best-effort: if realtime is unavailable (no LiveKit URL, no token command,
// connect error) each subscription is a clean no-op — the lists still
// refresh on their normal query lifecycle / screen focus.

import { useEffect, useRef } from "react";
import { router } from "expo-router";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import { invoke } from "../lib/native";
import { subscribeRealtime, type RealtimeSubscription } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import { dmQueryKeys, useDMChannels } from "./queries/useDMChannels";
import { groupQueryKeys, useUserGroupsWithChannels } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";
import {
  lastMessageQueryKeys,
  messageQueryKeys,
  useIngestConversation,
} from "./queries/useMessages";

export function useInboxRealtime() {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const queryClient = useQueryClient();
  const ingest = useIngestConversation();
  // The group/DM lists double as the realtime room list — the same queries
  // the tabs render from, so a membership change that refreshes the lists
  // also reconciles the subscriptions. No polling: list invalidation is
  // event-driven (below) or screen-focus.
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: dms = [] } = useDMChannels();

  // Read live values through refs so the (stable) event handler never forces
  // a resubscribe.
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;

  // One subscription per room, reconciled against the current room list —
  // rooms that persist across list changes keep their connection.
  const subsRef = useRef<Map<string, RealtimeSubscription>>(new Map());

  const roomIds: string[] = [];
  if (userId) {
    roomIds.push(`inbox-${userId}`);
    for (const g of groups) {
      // One LiveKit room per group covers all its channels; events carry the
      // channel_id in the payload.
      roomIds.push(g.id);
    }
    for (const d of dms) {
      roomIds.push(d.id);
    }
  }
  const roomsKey = roomIds.join(",");

  useEffect(() => {
    if (!userId) {
      return;
    }

    const handleEvent = (event: RealtimeEvent) => {
      const qc = qcRef.current;
      switch (event.type) {
        case "new_message": {
          // §5 signalling minimization: the payload carries conversation
          // routing only — no sender. LiveKit doesn't echo data to the
          // publishing participant, so our own sends don't arrive here; a
          // message from the same account's OTHER device does, and counts as
          // unread — same behaviour as desktop, whose own-sender check reads
          // a field this payload doesn't carry.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          const kind = event.channel_id ? "channel" : "dm";
          // Land the envelope, then refresh the message cache + previews
          // (ingest invalidates both).
          void ingestRef.current(id, kind);
          // Unread: increment unless the conversation is the one on screen.
          // Selection is set on row press and cleared when a list screen
          // regains focus on compact layouts, so "selected" tracks "open".
          const isOpen =
            appStore.selectedChannelId === id ||
            appStore.selectedConversationId === id;
          if (!isOpen) {
            appStore.incrementUnread(id);
          }
          break;
        }
        case "edited_message":
        case "deleted_message": {
          // Content changed without new mail — refresh the cache + previews,
          // never the unread count.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          void ingestRef.current(id, event.channel_id ? "channel" : "dm");
          void qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          break;
        }
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
        case "all_mention":
        case "user_mention": {
          // An @all or an @username (#843) in a group. Arrives on the per-user
          // inbox room so it reaches members even when they're not in the
          // group's LiveKit room. Ingest the referenced channel so the message
          // lands, then refresh that channel's message cache. Unread is left
          // to the accompanying new_message event so a connected client
          // doesn't double-count. (No foreground OS ping — mobile/lib/push
          // exposes no ready local-notification helper.)
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
                  // Without this the user is stranded on a dead signed-in
                  // screen after the self-sign-out (every command now fails).
                  router.replace("/(auth)/email");
                });
            })
            .catch((e) => {
              console.warn("[realtime] device_revoked check failed (ignored):", e);
            });
          break;
        }
        default:
          // Voice/typing/etc. events are not consumed on mobile; ignored.
          break;
      }
    };

    // Reconcile: subscribe rooms that appeared, close rooms that vanished.
    const subs = subsRef.current;
    const wanted = new Set(roomIds);
    for (const [room, sub] of subs) {
      if (!wanted.has(room)) {
        sub.close();
        subs.delete(room);
      }
    }
    for (const room of wanted) {
      if (!subs.has(room)) {
        subs.set(room, subscribeRealtime(room, handleEvent));
      }
    }

    return () => {
      // Only tear everything down when the user changes/signs out or the hook
      // unmounts — a room-list change re-runs the reconcile above instead.
      // (React runs this cleanup before the next effect; re-subscribing the
      // surviving rooms would drop and re-open their connections, so the
      // reconcile owns removal and this cleanup is a no-op between renders.)
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, roomsKey]);

  // Full teardown on sign-out / unmount.
  useEffect(() => {
    const subs = subsRef.current;
    if (!userId) {
      for (const sub of subs.values()) {
        sub.close();
      }
      subs.clear();
    }
    return () => {
      for (const sub of subs.values()) {
        sub.close();
      }
      subs.clear();
    };
  }, [userId]);
}
