// Opens the media viewer (#1248), always on the ROOT stack, so on iPad it covers the window.

import { useCallback } from "react";
import { useRouter } from "expo-router";
import type { ConversationKind } from "./queries";
import type { MessageAttachment } from "../types";
import type { PickedAttachment } from "../lib/attachments";
import { setDraftForViewer } from "../lib/composerAttachments";

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

/** The viewer on the composer's pending attachments (#1585), handed over in memory. */
export function useOpenDraftMediaViewer() {
  const router = useRouter();
  return useCallback(
    (picked: readonly PickedAttachment[], attachmentId: string) => {
      setDraftForViewer(picked);
      router.push({
        pathname: "/media",
        params: { source: "draft", attachmentId },
      });
    },
    [router],
  );
}
