// App-level foreground realtime for the signed-in user. Holds a data-only
// LiveKit subscription to:
//
//   - the personal inbox room (`inbox-${userId}`) — per-user events: a new DM
//     being created, group membership changes, @all/@username mentions,
//     device revocation, a new device asking to be approved (#1360);
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

import { useEffect, useRef, useState } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import { invoke } from "../lib/native";
import { useSegments } from "expo-router";
import { isRoomOpen, subscribeRealtime, type RealtimeSubscription } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import {
  catchUpTargets,
  roomCatchUpTarget,
  shouldCountUnread,
  type WakeLatest,
} from "../lib/messageWake";
import { dmQueryKeys, useDMChannels } from "./queries/useDMChannels";
import { groupQueryKeys, useUserGroupsWithChannels } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";
import {
  flushOutbox,
  lastMessageQueryKeys,
  messageQueryKeys,
  useIngestConversation,
} from "./queries/useMessages";
import { refreshUnread } from "./queries/useUnread";
import { deviceWasRevoked } from "../lib/deviceRevoked";
import { isInSignedInApp, noticeEnrollmentRequested } from "../lib/enrollmentPrompt";

export function useInboxRealtime() {
  const signedInId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  const inApp = !!signedInId && isInSignedInApp(segments, isLocked);
  // #1360: subscribe only once this device has landed in the signed-in app.
  // `currentUser` is set the moment the email code verifies — before the PIN,
  // the device cert, or (for a new device) enrollment — and a room token asked
  // for then is refused, because the device cannot sign yet. The failed room
  // was never retried, so the inbox (and with it every `enrollment_requested`
  // nudge) stayed dead for the whole session. Once in, stay subscribed through
  // a lock: a locked app still wants the nudges it will surface on unlock.
  const [userId, setUserId] = useState<string | null>(null);
  useEffect(() => {
    if (!signedInId) {
      setUserId(null);
    } else if (inApp) {
      setUserId(signedInId);
    }
  }, [signedInId, inApp]);
  const queryClient = useQueryClient();
  const ingest = useIngestConversation();
  // The group/DM lists double as the realtime room list — the same queries
  // the tabs render from, so a membership change that refreshes the lists
  // also reconciles the subscriptions. No polling: list invalidation is
  // event-driven (below) or screen-focus.
  const { data: groups = [], isSuccess: groupsLoaded } = useUserGroupsWithChannels();
  const { data: dms = [], isSuccess: dmsLoaded } = useDMChannels();

  // Read live values through refs so the (stable) event handler never forces
  // a resubscribe.
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;
  const groupsRef = useRef(groups);
  groupsRef.current = groups;
  const dmsRef = useRef(dms);
  dmsRef.current = dms;

  // One subscription per room, reconciled against the current room list —
  // rooms that persist across list changes keep their connection.
  const subsRef = useRef<Map<string, RealtimeSubscription>>(new Map());
  // The current event handler, for re-opening a room whose connect failed.
  const handlerRef = useRef<((event: RealtimeEvent) => void) | null>(null);

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
          // publishing participant, so this device's own sends don't arrive
          // here; a message from the same account's OTHER device does, and is
          // shown like anyone else's — but is never counted as unread, which
          // is only decidable once the envelope is decrypted.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          const kind = event.channel_id ? "channel" : "dm";
          // Unread: never for the conversation on screen. Selection is set on
          // row press and cleared when a list screen regains focus on compact
          // layouts; the chat screen also records itself as open, which covers
          // one reached without a row press (a notification tap). The open
          // chat advances its own read cursor as the message renders.
          const isOpen =
            appStore.selectedChannelId === id ||
            appStore.selectedConversationId === id ||
            appStore.openConversationId === id;
          // Land the envelope (ingest refreshes the message cache + previews),
          // then read the newest row back to see who wrote it. The bump below
          // is the instant badge; the persisted counts are re-read after it
          // (not by the ingest), so the two cannot add up to a double count.
          void ingestRef.current(id, kind, { refreshUnread: false }).then(async () => {
            if (isOpen) {
              refreshUnread(qc);
              return;
            }
            let latest: WakeLatest | null = null;
            try {
              const rows = await invoke<WakeLatest[]>("read_last_messages", {
                conversationIds: [id],
              });
              latest = rows?.[0] ?? null;
            } catch (e) {
              console.warn("[realtime] read_last_messages failed:", e);
            }
            if (shouldCountUnread({ latest, selfId: userId, isOpen })) {
              appStore.incrementUnread(id);
            }
            refreshUnread(qc);
          });
          break;
        }
        case "realtime_reconnected": {
          // The room's connection healed; whatever was published while it was
          // down is gone. Catch that room up.
          const target = roomCatchUpTarget(event.room, groupsRef.current, dmsRef.current);
          if (target) {
            void ingestRef.current(target.id, target.kind);
          }
          // The network is back, so whatever the outbox holds can go (#1335).
          void flushOutbox(qc);
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
          // A new DM — started by a peer, or started or accepted on another of
          // this user's devices (#1410) — refresh the DM list + requests, then
          // land what it already holds: until the list refetch subscribes its
          // room, nothing else would wake this device for those messages.
          void qc.invalidateQueries({ queryKey: dmQueryKeys.all });
          void ingestRef.current(event.conversation_id, "dm").then(() =>
            qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all }),
          );
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
              // The same sign-out as a command refused with `device_revoked`
              // (#1338): wipes the device, routes to sign-in, says why.
              deviceWasRevoked();
            })
            .catch((e) => {
              console.warn("[realtime] device_revoked check failed (ignored):", e);
            });
          break;
        }
        case "channel_mutes_changed":
          // Another device of this account muted or unmuted a channel
          // (#1431); the DS sent this. Re-read so the list dims to match.
          void qc.invalidateQueries({ queryKey: groupQueryKeys.mutedChannels(userId) });
          break;
        case "enrollment_requested":
          // A new device of this account filed its enrollment request (#1360).
          // The DS sends this, never a client. `useEnrollmentApprovals` opens
          // the approval screen for it (or holds it until the app is unlocked);
          // a QR-link request is skipped there, as desktop skips it.
          noticeEnrollmentRequested(event);
          break;
        default:
          // Voice/typing/etc. events are not consumed on mobile; ignored.
          break;
      }
    };

    handlerRef.current = handleEvent;

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

  // Resume catch-up. Every LiveKit wake-up that fired while the app was in
  // the background is gone, and a message sent from this user's OTHER device
  // has no push to fall back on (the DS never pushes your own message to your
  // own account) — so switching from the desktop back to the phone used to
  // show nothing new until each conversation was re-opened. Event-driven, not
  // polling: one pass per return to the foreground.
  useEffect(() => {
    if (!userId) {
      return;
    }
    let previous: AppStateStatus = AppState.currentState;
    const sub = AppState.addEventListener("change", (next) => {
      const resumed = previous !== "active" && next === "active";
      previous = next;
      if (!resumed) {
        return;
      }
      // A room whose connect failed (no network, a refused token) is gone
      // from the client; its handle here would otherwise stay dead until the
      // room list changed. Re-open it on the way back to the foreground.
      const handler = handlerRef.current;
      if (handler) {
        const subs = subsRef.current;
        for (const [room, sub] of subs) {
          if (!isRoomOpen(room)) {
            sub.close();
            subs.set(room, subscribeRealtime(room, handler));
          }
        }
      }
      for (const target of catchUpTargets(groupsRef.current, dmsRef.current)) {
        void ingestRef.current(target.id, target.kind);
      }
      // A send the OS suspended mid-flight, or one that failed while the
      // phone was offline, goes out now (#1335).
      void flushOutbox(qcRef.current);
    });
    return () => {
      sub.remove();
    };
  }, [userId]);

  // Landing in the app — cold start, sign-in, unlock — catches up every listed
  // conversation once, as a resume does (#1422). A message that arrived while
  // the app was closed or locked woke nothing here: no push runs JS in a killed
  // app, and LiveKit does not replay. Without this it stayed un-ingested, so
  // neither its preview nor its unread count showed until the conversation was
  // opened. Waits for both lists so a group or DM is not skipped; once per
  // landing, so a list refetch does not re-run it.
  const landedRef = useRef(false);
  useEffect(() => {
    if (!inApp) {
      landedRef.current = false;
      return;
    }
    if (landedRef.current || !groupsLoaded || !dmsLoaded) {
      return;
    }
    landedRef.current = true;
    for (const target of catchUpTargets(groupsRef.current, dmsRef.current)) {
      void ingestRef.current(target.id, target.kind);
    }
  }, [inApp, groupsLoaded, dmsLoaded]);

  // Signed in / unlocked: deliver what the durable outbox holds (#1335) — a
  // message a killed process left unsent. The core starts this drain itself
  // after unlock; calling it here waits for that and refreshes the rows it
  // settled, so a delivered message stops reading pending and a failed one
  // shows Retry. Once per session, event-driven.
  useEffect(() => {
    if (!userId) {
      return;
    }
    void flushOutbox(qcRef.current);
  }, [userId]);

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
