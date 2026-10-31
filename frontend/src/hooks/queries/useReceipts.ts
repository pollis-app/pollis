/** DM receipts (#857), per message and reader, from the local table; refreshed by realtime, never polled. */
import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { MessageReceipts } from "../../types";

export const receiptQueryKeys = {
  all: ["receipts"] as const,
  conversation: (conversationId: string) => ["receipts", conversationId] as const,
};

/** Receipts for one DM by message id; empty for null, channels, or the opt-out. */
export function useConversationReceipts(conversationId: string | null) {
  const query = useQuery({
    queryKey: receiptQueryKeys.conversation(conversationId ?? ""),
    enabled: !!conversationId,
    queryFn: async (): Promise<MessageReceipts[]> => {
      return await invoke<MessageReceipts[]>("get_conversation_receipts", {
        conversationId,
      });
    },
    staleTime: 1000 * 5,
  });

  // Stable per fetch, so the list's one-tick pick (#1637) recomputes only on new data.
  const byMessageId = useMemo(() => {
    const map = new Map<string, MessageReceipts>();
    for (const entry of query.data ?? []) {
      map.set(entry.message_id, entry);
    }
    return map;
  }, [query.data]);

  return { receipts: byMessageId, isLoading: query.isLoading };
}
