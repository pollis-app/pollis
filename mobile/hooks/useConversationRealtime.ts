// Live ingest for the open conversation on top of the chat screen's focus
// ingest, which stays the fallback. Best-effort: a no-op without realtime.

import { useEffect, useRef } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { subscribeRealtime } from "../lib/realtime/client";
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

  // Refs so the handler reads current values without forcing a reconnect.
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
    // One room per group covers all its channels; a DM's room is its own id.
    const roomName = kind === "channel" ? groupId : conversationId;
    if (!roomName) {
      return;
    }

    const handleEvent = (event: RealtimeEvent) => {
      // Membership is never announced in the timeline (#1506): data refresh only.
      if (event.type === "member_role_changed") {
        void qcRef.current.invalidateQueries({
          queryKey: groupInviteQueryKeys.members(event.group_id),
        });
        void qcRef.current.invalidateQueries({ queryKey: groupQueryKeys.all });
        return;
      }
      if (event.type === "roster_changed") {
        void qcRef.current.invalidateQueries({
          queryKey: groupInviteQueryKeys.members(event.conversation_id),
        });
        void qcRef.current.invalidateQueries({
          queryKey: groupInviteQueryKeys.groupInvites(event.conversation_id),
        });
        void qcRef.current.invalidateQueries({ queryKey: groupQueryKeys.all });
        void qcRef.current.invalidateQueries({ queryKey: dmQueryKeys.all });
        return;
      }
      if (event.type === "realtime_reconnected") {
        // Wake-ups published while the connection was down are lost; catch up.
        void ingestRef.current(conversationIdRef.current ?? conversationId, kind);
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
        // Any channel of this group, not only the one on screen.
        if (event.channel_id) {
          void ingestRef.current(event.channel_id, "channel");
        }
        return;
      }
      if (
        event.conversation_id &&
        event.conversation_id === conversationIdRef.current
      ) {
        void ingestRef.current(event.conversation_id, "dm");
      }
    };

    // Ref-counted, so a room the inbox already holds shares its connection.
    const sub = subscribeRealtime(roomName, handleEvent);
    return () => {
      sub.close();
    };
  }, [conversationId, kind, groupId]);
}
