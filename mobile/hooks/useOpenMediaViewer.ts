// Always a push on the root stack, so on iPad the viewer covers the whole
// window rather than the pane (`useNav` would push it there) (#1248).

import { useCallback } from "react";
import { useRouter } from "expo-router";
import type { ConversationKind } from "./queries";
import type { MessageAttachment } from "../types";

export function useOpenMediaViewer(
  conversationId: string | null,
  kind: ConversationKind | null,
  threadId?: string | null,
) {
  const router = useRouter();
  return useCallback(
    (attachment: MessageAttachment) => {
      if (!conversationId || !kind) {
        return;
      }
      router.push({
        pathname: "/media",
        params: {
          conversationId,
          kind,
          attachmentId: attachment.id,
          ...(threadId ? { threadId } : {}),
        },
      });
    },
    [router, conversationId, kind, threadId],
  );
}
