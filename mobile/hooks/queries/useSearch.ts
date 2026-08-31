// Cross-content search hook. Backs the (tabs)/search screen.
//
// Today we wire just `search_messages` — it goes through the local FTS
// index and returns a page of SearchResult rows. User search
// (`search_user_by_username`) is exposed via `useUserSearch` and groups can be
// filtered client-side from the cached `useUserGroupsWithChannels` list, so a
// single Rust invoke covers the typeahead.
//
// The types below mirror `pollis_core::commands::messages::types` field for
// field; the desktop copy lives in `frontend/src/types/index.ts`. Keep all
// three in sync.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

/** `sent_at DESC`, or bm25 with a recency decay. */
export type SearchSort = "recent" | "relevant";

/**
 * A snippet plus where to mark it. `highlights` are `[start, end)` pairs in
 * UTF-16 code units — plain JavaScript string indices — so a renderer slices
 * the text directly rather than being handed HTML.
 */
export interface SearchSnippet {
  text: string;
  highlights: [number, number][];
}

export interface SearchMessageResult {
  message_id: string;
  conversation_id: string;
  conversation_kind: "channel" | "dm" | null;
  conversation_name: string | null;
  group_id: string | null;
  group_name: string | null;
  sender_id: string;
  sender_username: string | null;
  thread_id: string | null;
  has_attachment: boolean;
  has_link: boolean;
  content: string;
  sent_at: string;
  snippet: SearchSnippet;
}

export interface SearchCursor {
  offset: number;
}

/**
 * What this device holds. Search only ever covers messages this device has
 * decrypted — bodies are E2EE, so the server has nothing to search — and these
 * are the numbers a UI needs to say so out loud.
 */
export interface SearchCorpus {
  message_count: number;
  earliest_sent_at: string | null;
  /** Device-local retention window in days; `0` is Forever. */
  retention_days: number;
  /** The one-time index backfill is still running. */
  indexing: boolean;
}

export interface SearchPage {
  results: SearchMessageResult[];
  total: number;
  next_cursor: SearchCursor | null;
  sort: SearchSort;
  corpus: SearchCorpus;
}

export const searchQueryKeys = {
  messages: (q: string) => ["search", "messages", q] as const,
};

export function useSearchMessages(query: string) {
  const trimmed = query.trim();
  return useQuery({
    queryKey: searchQueryKeys.messages(trimmed),
    queryFn: async (): Promise<SearchPage | null> => {
      if (!trimmed) {
        return null;
      }
      return await invoke<SearchPage>("search_messages", {
        query: trimmed,
        conversationId: null,
        sort: null,
        limit: 25,
        cursor: null,
      });
    },
    enabled: trimmed.length >= 2,
    staleTime: 1000 * 20,
  });
}
