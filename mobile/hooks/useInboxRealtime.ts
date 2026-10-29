// Foreground realtime for the signed-in user: data-only LiveKit subscriptions
// to the personal inbox room (`inbox-${userId}`, per-user events) and every
// group and DM room (message wake-ups, which the backend never publishes to
// the inbox). Rooms are ref-counted in `lib/realtime/client`, so the open
// chat's subscription shares a connection. Best-effort: without realtime each
// subscription is a no-op. See mobile/CLAUDE.md → "Foreground realtime".

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
import { catchUpThenBackstop } from "../lib/mlsBackstop";

export function useInboxRealtime() {
  const signedInId = useObserver(() => appStore.currentUser?.id ?? null);
  const isLocked = useObserver(() => appStore.isLocked);
  const segments = useSegments();
  const inApp = !!signedInId && isInSignedInApp(segments, isLocked);
  // #1360: subscribe only once in the signed-in app. `currentUser` is set at
  // email verification, before the device can sign a room-token request, and a
  // refused room was never retried. Stay subscribed through a lock: a locked
  // app still wants the nudges it will surface on unlock.
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
  // The lists the tabs render double as the room list, so a membership change
  // that refreshes them also reconciles the subscriptions.
  const { data: groups = [], isSuccess: groupsLoaded } = useUserGroupsWithChannels();
  const { data: dms = [], isSuccess: dmsLoaded } = useDMChannels();

  // Refs, so the stable event handler never forces a resubscribe.
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;
  const groupsRef = useRef(groups);
  groupsRef.current = groups;
  const dmsRef = useRef(dms);
  dmsRef.current = dms;

  // Rooms that persist across list changes keep their connection.
  const subsRef = useRef<Map<string, RealtimeSubscription>>(new Map());
  // For re-opening a room whose connect failed.
  const handlerRef = useRef<((event: RealtimeEvent) => void) | null>(null);

  const roomIds: string[] = [];
  if (userId) {
    roomIds.push(`inbox-${userId}`);
    for (const g of groups) {
      // One room per group covers all its channels.
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
          // The payload carries routing only, no sender (§5). LiveKit doesn't echo
          // this device's own sends; one from the account's other device arrives and is
          // shown, but whether it is unread is only decidable after decryption.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          const kind = event.channel_id ? "channel" : "dm";
          // Never unread for the conversation on screen: selection covers a row press,
          // `openConversationId` a chat reached otherwise (a notification tap).
          const isOpen =
            appStore.selectedChannelId === id ||
            appStore.selectedConversationId === id ||
            appStore.openConversationId === id;
          // The bump below is the instant badge; the persisted counts are re-read
          // after it, not by the ingest, so the two cannot double count.
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
          // Whatever was published while the room was down is gone: catch it up.
          const target = roomCatchUpTarget(event.room, groupsRef.current, dmsRef.current);
          if (target) {
            void ingestRef.current(target.id, target.kind);
          }
          // The network is back, so the outbox can drain (#1335).
          void flushOutbox(qc);
          break;
        }
        case "edited_message":
        case "deleted_message": {
          // Content changed without new mail: never the unread count.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          void ingestRef.current(id, event.channel_id ? "channel" : "dm");
          void qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          break;
        }
        case "dm_created":
          // From a peer, or started or accepted on another of our devices (#1410).
          // Land what it holds now: until the list refetch subscribes its room,
          // nothing else would wake this device for those messages.
          void qc.invalidateQueries({ queryKey: dmQueryKeys.all });
          void ingestRef.current(event.conversation_id, "dm").then(() =>
            qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all }),
          );
          break;
        case "membership_changed":
          void qc.invalidateQueries({ queryKey: groupQueryKeys.all });
          void qc.invalidateQueries({
            queryKey: groupInviteQueryKeys.pending(userId),
          });
          break;
        case "all_mention":
        case "user_mention": {
          // @all / @username (#843), on the inbox so it reaches members outside the
          // group's room. Unread is left to the accompanying new_message so a
          // connected client doesn't double-count.
          void ingestRef.current(event.channel_id, "channel");
          void qc.invalidateQueries({
            queryKey: messageQueryKeys.conversation(event.channel_id, "channel"),
          });
          break;
        }
        case "device_revoked": {
          // The payload is advisory and spoofable: confirm THIS device is gone
          // before signing out. An error counts as still registered, so a network
          // blip never signs anyone out.
          if (!userId) {
            break;
          }
          void invoke<boolean>("is_current_device_registered", { userId })
            .then((stillRegistered) => {
              if (stillRegistered) {
                return;
              }
              console.warn("[realtime] this device was revoked — signing out");
              // The same sign-out as a command refused with `device_revoked` (#1338).
              deviceWasRevoked();
            })
            .catch((e) => {
              console.warn("[realtime] device_revoked check failed (ignored):", e);
            });
          break;
        }
        case "channel_mutes_changed":
          // Another device of this account (un)muted a channel (#1431).
          void qc.invalidateQueries({ queryKey: groupQueryKeys.mutedChannels(userId) });
          break;
        case "enrollment_requested":
          // From the DS, never a client (#1360); `useEnrollmentApprovals` opens it.
          noticeEnrollmentRequested(event);
          break;
        default:
          break;
      }
    };

    handlerRef.current = handleEvent;

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
      // The reconcile above owns room removal; tearing down here would drop and
      // re-open every surviving room on each list change.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, roomsKey]);

  // Resume catch-up: wake-ups fired while backgrounded are gone, and a message
  // from this user's other device has no push to fall back on. One pass per
  // return to the foreground.
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
      // A room whose connect failed would stay dead until the room list changed.
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
      // The re-add backstop (#1571): a device missing from a conversation's tree
      // is put back now, not when it next opens that conversation.
      const resumedUser = userId;
      void catchUpThenBackstop(
        catchUpTargets(groupsRef.current, dmsRef.current),
        (id, kind) => ingestRef.current(id, kind),
        () => invoke("reconcile_backstop_all", { userId: resumedUser }),
      );
      void flushOutbox(qcRef.current);
    });
    return () => {
      sub.remove();
    };
  }, [userId]);

  // Landing in the app (cold start, sign-in, unlock) catches up every listed
  // conversation once (#1422): a message that arrived while closed or locked
  // woke nothing, and LiveKit does not replay. Waits for both lists.
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
    const landedUser = signedInId;
    void catchUpThenBackstop(
      catchUpTargets(groupsRef.current, dmsRef.current),
      (id, kind) => ingestRef.current(id, kind),
      () => invoke("reconcile_backstop_all", { userId: landedUser }),
    );
  }, [inApp, groupsLoaded, dmsLoaded, signedInId]);

  // Deliver what a killed process left in the outbox (#1335). The core drains
  // after unlock itself; this waits for that and refreshes the settled rows.
  useEffect(() => {
    if (!userId) {
      return;
    }
    void flushOutbox(qcRef.current);
  }, [userId]);

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
