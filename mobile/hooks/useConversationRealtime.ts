// Foreground realtime for the open conversation.
//
// Supplements the focus-effect ingest in the chat screen: while a chat is
// open we also hold a LiveKit data-channel connection to the conversation's
// room, and re-run the same envelope ingest the moment a relevant event
// arrives — so a message sent by a peer shows up without waiting for the
// next screen focus. The focus-effect ingest remains the fallback and the
// catch-up-after-background path; this hook only adds liveness on top.
//
// Entirely best-effort: if realtime is unavailable (no LiveKit URL, no
// token command, connect error) `connectRealtime` returns null and this
// hook is a clean no-op.

import { useEffect, useRef } from "react";
import type { Room } from "livekit-client";
import { useQueryClient } from "@tanstack/react-query";
import { connectRealtime, disconnectRealtime } from "../lib/realtime/client";
import type { RealtimeEvent } from "../lib/realtime/events";
import { useIngestConversation, type ConversationKind } from "./queries/useMessages";
import { groupQueryKeys } from "./queries/useUserGroups";
import { groupInviteQueryKeys } from "./queries/useGroupInvites";
import { dmQueryKeys } from "./queries/useDMChannels";

export function useConversationRealtime(
  conversationId: string | null,
  kind: ConversationKind | null,
  groupId?: string,
) {
  const ingest = useIngestConversation();
  const queryClient = useQueryClient();

  // Refs keep the (stable-identity) data handler reading current values
  // without forcing a reconnect on every render.
  const ingestRef = useRef(ingest);
  ingestRef.current = ingest;
  const qcRef = useRef(queryClient);
  qcRef.current = queryClient;
  const conversationIdRef = useRef(conversationId);
  conversationIdRef.current = conversationId;

  useEffect(() => {
    if (!conversationId || !kind) {
      return;
    }
    // A GROUP uses one room (its group_id) covering all its channels; a DM
    // uses a room named by its conversation_id.
    const roomName = kind === "channel" ? groupId : conversationId;
    if (!roomName) {
      return;
    }

    let cancelled = false;
    let room: Room | null = null;

    const handleEvent = (event: RealtimeEvent) => {
      // Membership / role events arrive on the conversation (group) room.
      // Data-refresh parity only — invalidate the affected queries so the
      // member list and group/DM lists reflect live changes. Banner UX
      // (desktop's ephemeral rosterChangeStore notices) is future work.
      if (event.type === "member_role_changed") {
        // Mirror desktop: refresh the affected group's member list and the
        // groups list (which embeds the current user's role).
        void qcRef.current.invalidateQueries({
          queryKey: groupInviteQueryKeys.members(event.group_id),
        });
        void qcRef.current.invalidateQueries({ queryKey: groupQueryKeys.all });
        return;
      }
      if (event.type === "roster_changed") {
        // Mirror desktop's INTENT (data only): refresh the affected
        // conversation's member/detail queries plus the groups and DM lists
        // so membership reflects live. The ephemeral roster BANNER UI is a
        // separate enhancement, not ported here.
        void qcRef.current.invalidateQueries({
          queryKey: groupInviteQueryKeys.members(event.conversation_id),
        });
        void qcRef.current.invalidateQueries({ queryKey: groupQueryKeys.all });
        void qcRef.current.invalidateQueries({ queryKey: dmQueryKeys.all });
        return;
      }
      if (
        event.type !== "new_message" &&
        event.type !== "edited_message" &&
        event.type !== "deleted_message"
      ) {
        return;
      }
      if (kind === "channel") {
        // Any channel_id arriving on the group room belongs to this group;
        // ingest that specific channel so its message cache refreshes even
        // if it isn't the one currently on screen.
        if (event.channel_id) {
          void ingestRef.current(event.channel_id, "channel");
        }
        return;
      }
      // DM room: only events for the open conversation are relevant.
      if (
        event.conversation_id &&
        event.conversation_id === conversationIdRef.current
      ) {
        void ingestRef.current(event.conversation_id, "dm");
      }
    };

    connectRealtime(roomName, handleEvent)
      .then((connected) => {
        if (cancelled) {
          // Unmounted (or ids changed) before connect resolved.
          disconnectRealtime(connected);
          return;
        }
        room = connected;
      })
      .catch((e) => {
        console.warn("[realtime] useConversationRealtime connect error:", e);
      });

    return () => {
      cancelled = true;
      disconnectRealtime(room);
      room = null;
    };
  }, [conversationId, kind, groupId]);
}
