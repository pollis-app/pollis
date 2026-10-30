// Re-read a conversation the moment the core stores rows in it (#1610), whoever stored them: the
// conversation's own ingest, a catch-up sweep, or Android's in-process push fetch. One bridge call
// stays parked in the core until something is stored, so this is event-driven, not polling.

import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { invoke } from "../lib/native";
import { messageQueryKeys, refreshStoredConversations } from "./queries/useMessages";

// Each waiter keeps its own cursor in the core, so a loop left parked across a sign-out sees
// the same notes as the new one and can never take them from it.
interface Persisted {
  seq: number;
  conversationIds: string[];
  everything: boolean;
}

export function usePersistedFeed(userId: string | null) {
  const queryClient = useQueryClient();

  useEffect(() => {
    if (!userId) {
      return;
    }
    let stopped = false;
    void (async () => {
      let after = 0;
      while (!stopped) {
        let got: Persisted;
        try {
          got = await invoke<Persisted>("wait_persisted", { after });
        } catch (e) {
          // Only a bridge failure lands here; the ingest's own invalidation still covers every
          // screen, so stop rather than spin. The next sign-in starts a new feed.
          console.warn("[persisted] feed stopped:", e);
          return;
        }
        if (stopped) {
          return;
        }
        after = got.seq;
        if (got.everything) {
          // Our cursor predates a reset of the core's list: re-read every conversation.
          void queryClient.invalidateQueries({ queryKey: messageQueryKeys.all, refetchType: "all" });
        }
        refreshStoredConversations(queryClient, got.conversationIds);
      }
    })();
    return () => {
      // A parked call cannot be cancelled; it returns on the next store write and exits here.
      stopped = true;
    };
  }, [userId, queryClient]);
}
