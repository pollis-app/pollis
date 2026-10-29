/** DM receipts (#857), per message and reader, from the local table; refreshed by realtime, never polled. */
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

  const byMessageId = new Map<string, MessageReceipts>();
  for (const entry of query.data ?? []) {
    byMessageId.set(entry.message_id, entry);
  }

  return { receipts: byMessageId, isLoading: query.isLoading };
}
