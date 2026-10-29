// DM receipts (#892): one fetch per open conversation, keyed by message id; the reciprocal rule is enforced in Rust.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export interface MessageReceipts {
  message_id: string;
  /** Readers whose device decrypted it. */
  delivered_by: string[];
  /** Readers who saw it on screen; a subset of `delivered_by`. */
  read_by: string[];
}

export const receiptQueryKeys = {
  all: ["receipts"] as const,
  conversation: (conversationId: string | null) =>
    ["receipts", conversationId] as const,
};

export function useConversationReceipts(conversationId: string | null) {
  return useQuery({
    queryKey: receiptQueryKeys.conversation(conversationId),
    queryFn: async (): Promise<Map<string, MessageReceipts>> => {
      const map = new Map<string, MessageReceipts>();
      if (!conversationId) {
        return map;
      }
      const rows =
        (await invoke<MessageReceipts[]>("get_conversation_receipts", {
          conversationId,
        })) ?? [];
      for (const row of rows) {
        map.set(row.message_id, row);
      }
      return map;
    },
    enabled: !!conversationId,
    staleTime: 1000 * 5,
  });
}
