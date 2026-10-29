// Data-only LiveKit subscriptions for the signed-in user: the personal inbox
// room (`inbox-${userId}`) for per-user events, and every group / DM room for
// message wake-ups, which are published to the conversation's room, never the
// inbox. Best-effort: with realtime unavailable each subscription is a no-op.

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
  // Subscribe only once in the signed-in app (#1360): `currentUser` is set at
  // email verification, before the device can sign a room-token request. Stay
  // subscribed through a lock so nudges surface on unlock.
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
  // The group/DM lists double as the room list, so a membership change that
  // refreshes them also reconciles the subscriptions.
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

  const subsRef = useRef<Map<string, RealtimeSubscription>>(new Map());
  const handlerRef = useRef<((event: RealtimeEvent) => void) | null>(null);

  const roomIds: string[] = [];
  if (userId) {
    roomIds.push(`inbox-${userId}`);
    for (const g of groups) {
      // One LiveKit room per group covers all its channels.
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
          // The payload carries no sender, so whether this is our own other
          // device's message (never unread) is decided after decryption.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          const kind = event.channel_id ? "channel" : "dm";
          // Never unread for the conversation on screen.
          const isOpen =
            appStore.selectedChannelId === id ||
            appStore.selectedConversationId === id ||
            appStore.openConversationId === id;
          // The persisted counts are re-read after the instant bump, not by
          // the ingest, so the two cannot double count.
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
          // Whatever was published while the room was down is gone; catch up.
          const target = roomCatchUpTarget(event.room, groupsRef.current, dmsRef.current);
          if (target) {
            void ingestRef.current(target.id, target.kind);
          }
          void flushOutbox(qc);
          break;
        }
        case "edited_message":
        case "deleted_message": {
          // Never touches the unread count.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          void ingestRef.current(id, event.channel_id ? "channel" : "dm");
          void qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          break;
        }
        case "dm_created":
          // Ingest now: until the list refetch subscribes its room, nothing
          // else would wake this device for its messages (#1410).
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
          // Unread is left to the accompanying new_message so it isn't double counted (#843).
          void ingestRef.current(event.channel_id, "channel");
          void qc.invalidateQueries({
            queryKey: messageQueryKeys.conversation(event.channel_id, "channel"),
          });
          break;
        }
        case "device_revoked": {
          // The payload is advisory and spoofable: confirm with the backend
          // that THIS device is gone before signing out. An error counts as
          // "still registered", so a blip never signs out.
          if (!userId) {
            break;
          }
          void invoke<boolean>("is_current_device_registered", { userId })
            .then((stillRegistered) => {
              if (stillRegistered) {
                return;
              }
              console.warn("[realtime] this device was revoked — signing out");
              deviceWasRevoked();
            })
            .catch((e) => {
              console.warn("[realtime] device_revoked check failed (ignored):", e);
            });
          break;
        }
        case "channel_mutes_changed":
          void qc.invalidateQueries({ queryKey: groupQueryKeys.mutedChannels(userId) });
          break;
        case "enrollment_requested":
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
      // No-op: the reconcile above owns removal, so surviving rooms keep
      // their connection across a room-list change.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, roomsKey]);

  // Resume catch-up: wake-ups fired while backgrounded are gone, and a message
  // from this user's other device has no push to fall back on.
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
      // A room whose connect failed would otherwise stay dead until the room
      // list changed.
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
      void flushOutbox(qcRef.current);
    });
    return () => {
      sub.remove();
    };
  }, [userId]);

  // Landing in the app (cold start, sign-in, unlock) catches up every listed
  // conversation once, after both lists load (#1422).
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

  // The core drains the outbox after unlock; this waits for it and refreshes
  // the rows it settled (#1335).
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
