// App-level foreground realtime: the inbox room plus every group and DM room. Best-effort; rooms are ref-counted in lib/realtime/client.

import { useEffect, useRef, useState } from "react";
import { AppState, type AppStateStatus } from "react-native";
import { useObserver } from "mobx-react-lite";
import { useQueryClient } from "@tanstack/react-query";
import { appStore } from "../stores/appStore";
import { invoke } from "../lib/native";
import { useSegments } from "expo-router";
import { subscribeRealtime, type RealtimeSubscription } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import { catchUpTargets, roomCatchUpTarget } from "../lib/messageWake";
import { dmQueryKeys, useDMChannels } from "./queries/useDMChannels";
import { groupQueryKeys, useUserGroupsWithChannels } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";
import { aliasQueryKeys } from "./queries/useGroupAliases";
import { botQueryKeys } from "./queries/useBots";
import { botAppQueryKeys } from "./queries/useBotApps";
import {
  flushOutbox,
  lastMessageQueryKeys,
  messageQueryKeys,
  useIngestConversation,
} from "./queries/useMessages";
import { refreshReadCursors } from "./queries/useUnread";
import { onProfileChanged, refreshProfile } from "./queries/useUserProfile";
import { deviceWasRevoked } from "../lib/deviceRevoked";
import { isInSignedInApp, noticeEnrollmentRequested } from "../lib/enrollmentPrompt";
import { catchUpThenBackstop } from "../lib/mlsBackstop";
import { noticeHistoryTransfer } from "../lib/historyTransferPrompt";

/**
 * Re-subscribe every room whose subscription no longer hears it (its connect failed). Asked of the
 * subscription, not the room: the open chat may have re-opened the same room since, and that
 * entry does not carry this hook's listener, so the inbox would never hear its nudges (#2084).
 */
function reopenDropped(
  subs: Map<string, RealtimeSubscription>,
  handler: ((event: RealtimeEvent) => void) | null,
): void {
  if (!handler) {
    return;
  }
  for (const [room, sub] of subs) {
    if (!sub.isOpen()) {
      sub.close();
      subs.set(room, subscribeRealtime(room, handler));
    }
  }
}

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
  const {
    data: groups = [],
    isSuccess: groupsLoaded,
    dataUpdatedAt: groupsReadAt,
  } = useUserGroupsWithChannels();
  const { data: dms = [], isSuccess: dmsLoaded, dataUpdatedAt: dmsReadAt } = useDMChannels();

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
          // No sender in the payload (§5), and it may be only a receipt (#1628): ingest, then
          // re-read the counts Rust derives from the read cursor. Nothing is bumped locally, so a
          // receipt or a message already read on another device never badges.
          const id = event.channel_id ?? event.conversation_id;
          if (!id) {
            break;
          }
          void ingestRef.current(id, event.channel_id ? "channel" : "dm");
          break;
        }
        case "realtime_reconnected": {
          const target = roomCatchUpTarget(event.room, groupsRef.current, dmsRef.current);
          if (target) {
            void ingestRef.current(target.id, target.kind);
          }
          void flushOutbox(qc);
          // #1628: reads made elsewhere during the outage; their nudge was lost too.
          void refreshReadCursors(userId, qc);
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
          // A room refused before this membership landed can be joined now (#2084).
          reopenDropped(subsRef.current, handlerRef.current);
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
        case "group_aliases_changed":
          // #2003: one of our own aliases changed (another device, or an admin reset).
          void qc.invalidateQueries({ queryKey: aliasQueryKeys.all });
          break;
        case "bots_changed":
          // #1182: a host joined one of this admin's bots; an open Bots page re-reads.
          void qc.invalidateQueries({ queryKey: botQueryKeys.all });
          // #2005: an approval or the runtime's report changed; the bot's App section re-reads.
          void qc.invalidateQueries({ queryKey: botAppQueryKeys.all });
          break;
        case "read_cursors_changed":
          // #1628: another device read something; converge, re-badge, clear its notifications.
          void refreshReadCursors(userId, qc);
          break;
        case "profile_changed":
          // #2084: someone in these rooms (or another of our devices) renamed; re-read and redraw.
          void onProfileChanged(qc, event.user_id);
          break;
        case "enrollment_requested":
          // #1360; `useEnrollmentApprovals` opens the approval screen.
          noticeEnrollmentRequested(event);
          break;
        case "history_transfer":
          // #1735, content-free; `useHistoryTransferService` re-reads and runs.
          noticeHistoryTransfer({ transfer_id: event.transfer_id, event: event.event });
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
    // A room refused its token because the membership had not landed yet (a group being
    // joined, #2084) gets another try each time the lists are re-read, which the join itself
    // causes: event-driven, never a timer.
    reopenDropped(subs, handleEvent);

    return () => {
      // The reconcile owns removal; this cleanup is a no-op between renders.
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [userId, roomsKey, groupsReadAt, dmsReadAt]);

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
      // Counts and the cross-device cursor converge are `useUnreadSync`'s resume handler.
      // Re-open rooms whose connect failed.
      reopenDropped(subsRef.current, handlerRef.current);
      // Then the re-add and self-update backstops (#1571, #1880).
      const resumedUser = userId;
      void catchUpThenBackstop(
        catchUpTargets(groupsRef.current, dmsRef.current),
        (id, kind) => ingestRef.current(id, kind),
        () => invoke("reconcile_backstop_all", { userId: resumedUser }),
      );
      void flushOutbox(qcRef.current);
      // #2084: a rename made on another device while we were away.
      void refreshProfile(resumedUser);
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
    // #2084: the session's handle came from the accounts index; a rename elsewhere corrects it.
    if (landedUser) {
      void refreshProfile(landedUser);
    }
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
