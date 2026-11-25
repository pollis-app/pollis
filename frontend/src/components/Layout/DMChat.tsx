import React, { useEffect, useRef } from "react";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { useMarkConversationRead } from "../../hooks/queries/useUnread";
import { useReadReceipts } from "../../hooks/useReadReceipts";
import { useSendReadReceipts } from "../../hooks/queries/usePreferences";
import { MainContent, type PendingDmRequest } from "./MainContent";

/** The mounted view that last claimed the selection; a view leaving clears it only if it is still this one. */
let selectionOwner: object | null = null;

interface DMChatProps {
  conversationId: string;
  pendingDmRequest?: PendingDmRequest | null;
  autoFocus?: boolean;
}

/**
 * The one DM chat view (#2116): the DM page's body and the call's right panel. Mounted, it is the
 * conversation on screen: the selection the realtime wake and `useReadWhileViewing` read (#2056),
 * a durable read cursor on open, and read receipts (#857).
 */
export const DMChat: React.FC<DMChatProps> = observer(
  ({ conversationId, pendingDmRequest = null, autoFocus = true }) => {
    const userId = appStore.currentUser?.id ?? null;
    const { mutate: markConversationRead } = useMarkConversationRead(userId);
    const sendReadReceipts = useSendReadReceipts();
    const token = useRef({}).current;

    useEffect(() => {
      selectionOwner = token;
      appStore.setSelectedConversationId(conversationId);
      // Clear on any nav path, with a durable cursor (#844).
      markConversationRead(conversationId);
      return () => {
        // Only our own claim: the next view (the call's panel, on the same DM) may already hold it.
        if (selectionOwner !== token) {
          return;
        }
        selectionOwner = null;
        // A channel page may have taken the selection since.
        if (appStore.selectedConversationId === conversationId) {
          appStore.setSelectedConversationId(null);
        }
      };
    }, [conversationId, markConversationRead, token]);

    // Receipts (#857) are DM-only and track their own viewport + focus; Rust enforces the preference.
    useReadReceipts(conversationId, userId, sendReadReceipts);

    return <MainContent pendingDmRequest={pendingDmRequest} autoFocus={autoFocus} />;
  },
);
