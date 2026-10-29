/**
 * DM delivery / read receipts (#857), device-local per message and reader.
 * No polling: receipts arrive as MLS control envelopes, refreshed by the same
 * realtime hint as messages.
 */
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { MessageReceipts } from "../../types";

export const receiptQueryKeys = {
  all: ["receipts"] as const,
  conversation: (conversationId: string) => ["receipts", conversationId] as const,
};

/**
 * Every receipt for one DM, keyed by message id. Empty for group channels and
 * when the user opted out (Rust then stores none).
 */
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
