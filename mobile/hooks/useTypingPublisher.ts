// Sends this user's typing signal (#1635) on the open conversation's room, as desktop does.

import { useEffect, useMemo } from "react";
import { publishRealtime } from "../lib/realtime/client";
import { createTypingThrottle, typingPayload } from "../lib/typing";
import type { ConversationKind } from "./queries/useMessages";

/** `notify(draft)` on every change; `stop()` on send. Stops on unmount and on a conversation switch. */
export function useTypingPublisher(
  conversationId: string | null,
  kind: ConversationKind | null,
  groupId?: string,
) {
  // A group uses one room (its group_id); a DM its conversation_id.
  const roomName = kind === "channel" ? groupId ?? null : conversationId;
  const channelId = kind === "channel" ? conversationId : null;
  const dmId = kind === "dm" ? conversationId : null;

  const throttle = useMemo(
    () =>
      createTypingThrottle({
        publish: (isTyping) => {
          if (!roomName || (!channelId && !dmId)) {
            return;
          }
          publishRealtime(roomName, typingPayload(channelId, dmId, isTyping), false);
        },
      }),
    [roomName, channelId, dmId],
  );

  useEffect(() => {
    return () => {
      throttle.stop();
    };
  }, [throttle]);

  return throttle;
}
