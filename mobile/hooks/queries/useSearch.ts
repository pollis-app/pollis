// Cross-content search hook. Backs the (tabs)/search screen.
//
// Today we wire just `search_messages` — it goes through the local FTS
// index and returns SearchResult rows. User search (`search_user_by_username`)
// is exposed via `useUserSearch` and groups can be filtered client-side
// from the cached `useUserGroupsWithChannels` list, so a single Rust
// invoke covers the typeahead.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export interface SearchMessageResult {
  message_id: string;
  conversation_id: string;
  sender_id: string;
  content: string;
  sent_at: string;
  snippet: string;
}

export const searchQueryKeys = {
  messages: (q: string) => ["search", "messages", q] as const,
};

export function useSearchMessages(query: string) {
  const trimmed = query.trim();
  return useQuery({
    queryKey: searchQueryKeys.messages(trimmed),
    queryFn: async (): Promise<SearchMessageResult[]> => {
      if (!trimmed) {
        return [];
      }
      return await invoke<SearchMessageResult[]>("search_messages", {
        query: trimmed,
        limit: 50,
      });
    },
    enabled: trimmed.length >= 2,
    staleTime: 1000 * 20,
  });
}
