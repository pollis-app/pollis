// Message read hook. Mirrors `frontend/src/hooks/queries/useMessages.ts`
// for the chat screen's read path. Send / ingest / pagination come later
// — this hook covers the "open a chat, see the recent messages" flow.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export interface Message {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  content: string;
  reply_to_id?: string | null;
  created_at: number | string;
  updated_at: number | string;
}

export const messageQueryKeys = {
  all: ["messages"] as const,
  conversation: (conversationId: string | null) =>
    ["messages", "conversation", conversationId] as const,
};

export function useMessages(
  conversationId: string | null,
  opts?: { limit?: number },
) {
  return useQuery({
    queryKey: messageQueryKeys.conversation(conversationId),
    queryFn: async (): Promise<Message[]> => {
      if (!conversationId) {
        return [];
      }
      const messages = await invoke<Message[]>("list_messages", {
        conversationId,
        limit: opts?.limit ?? 50,
      });
      return messages ?? [];
    },
    enabled: !!conversationId,
    staleTime: 1000 * 30,
  });
}
