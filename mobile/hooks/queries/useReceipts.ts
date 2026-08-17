// DM delivery/read receipt read hook (#892). Mirrors desktop's
// `useConversationReceipts` (frontend/src/hooks/queries/useReceipts.ts):
// one `get_conversation_receipts` fetch per open conversation, keyed into a
// map by message id. Messages with no receipts are absent. The core returns
// an empty list when the user's `send_read_receipts` preference is off —
// the reciprocal rule (your own flag gates both emitting AND recording) is
// enforced in Rust, not here.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

/** Mirror of `pollis_core::commands::messages::receipts::MessageReceipts`. */
export interface MessageReceipts {
  message_id: string;
  /** Readers whose device fetched and decrypted the message. */
  delivered_by: string[];
  /** Readers who actually saw it on screen. Subset of `delivered_by`. */
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
