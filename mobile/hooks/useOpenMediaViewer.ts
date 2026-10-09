// Opens the full-screen media viewer (#1248) on an attachment. Always a push
// on the ROOT stack — on iPad too, where the conversation sits in the
// two-pane's detail pane: the viewer covers the whole window rather than
// opening inside the pane (`useNav` would push it there).

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
