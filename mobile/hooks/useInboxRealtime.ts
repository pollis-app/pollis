// App-level foreground realtime: the inbox room plus every group and DM room. Best-effort; rooms are ref-counted in lib/realtime/client.

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
  // #1360: subscribe only once in the signed-in app; a room token asked for before the device can sign is refused.
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
  // The lists double as the realtime room list. No polling.
  const { data: groups = [], isSuccess: groupsLoaded } = useUserGroupsWithChannels();
  const { data: dms = [], isSuccess: dmsLoaded } = useDMChannels();

  // Refs so the stable event handler never forces a resubscribe.
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
          // No sender in the payload (§5); own-account messages are never counted unread.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          const kind = event.channel_id ? "channel" : "dm";
          // Unread: never for the conversation on screen.
          const isOpen =
            appStore.selectedChannelId === id ||
            appStore.selectedConversationId === id ||
            appStore.openConversationId === id;
          // The bump below is the instant badge; counts are re-read after it, so they cannot double.
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
          const target = roomCatchUpTarget(event.room, groupsRef.current, dmsRef.current);
          if (target) {
            void ingestRef.current(target.id, target.kind);
          }
          void flushOutbox(qc);
          break;
        }
        case "edited_message":
        case "deleted_message": {
          // Never the unread count.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          void ingestRef.current(id, event.channel_id ? "channel" : "dm");
          void qc.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
          break;
        }
        case "dm_created":
          // Also from this user's other devices (#1410); land it before the list refetch subscribes the room.
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
          // An @all or @username (#843); unread is left to the new_message event.
          void ingestRef.current(event.channel_id, "channel");
          void qc.invalidateQueries({
            queryKey: messageQueryKeys.conversation(event.channel_id, "channel"),
          });
          break;
        }
        case "device_revoked": {
          // The payload is spoofable: confirm with the backend; any error counts as still registered.
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
          // #1360; `useEnrollmentApprovals` opens the approval screen.
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
      // The reconcile owns removal; this cleanup is a no-op between renders.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, roomsKey]);

  // Resume catch-up: own-device messages have no push to fall back on. One pass per foreground.
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
      // Rows stored while away (iOS's notification extension stores each pushed message) show
      // now, from the local store, before any round trip (#1610).
      void qcRef.current.invalidateQueries({ queryKey: messageQueryKeys.all, refetchType: "all" });
      void qcRef.current.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
      refreshUnread(qcRef.current);
      // Re-open rooms whose connect failed.
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
      // Then the re-add backstop (#1571).
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

  // Landing in the app catches up every listed conversation once (#1422); no push runs JS in a killed app.
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

  // Signed in / unlocked: drain the durable outbox (#1335), once per session.
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
