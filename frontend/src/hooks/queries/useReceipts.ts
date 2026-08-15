/**
 * Delivery / read receipts for DMs (#857).
 *
 * Receipts live in the device-local `message_receipt` table, per message and
 * per reader — never a single boolean — so a DM with several members renders
 * "3 of 5 read" from the same data a 1:1 DM renders a single tick from.
 *
 * Nothing here polls. Receipts arrive as MLS control envelopes on the ordinary
 * message path, so the same realtime hint that triggers a message ingest also
 * refreshes this query (see `useLiveKitRealtime`).
 */
import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { MessageReceipts } from "../../types";

export const receiptQueryKeys = {
  all: ["receipts"] as const,
  conversation: (conversationId: string) => ["receipts", conversationId] as const,
};

/**
 * Every receipt this device holds for one DM, keyed by message id for O(1)
 * lookup while rendering a list.
 *
 * Returns an empty map for a null conversation, for group channels (which never
 * have receipts), and while the user's reciprocal opt-out is off — in the last
 * case because the Rust side genuinely stores none, not because the UI is
 * hiding them.
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
