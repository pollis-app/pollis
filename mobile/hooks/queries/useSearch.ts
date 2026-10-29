// Types mirror `pollis_core::commands::messages::types` and the desktop copy in
// `frontend/src/types/index.ts`; keep all three in sync.

import { useInfiniteQuery } from "@tanstack/react-query";
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

/** What this device holds: search covers only messages it has decrypted. */
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
  /** Every message search — invalidated when the local message store changes. */
  all: ["search"] as const,
  messages: (q: string, sort: SearchSort | null) =>
    ["search", "messages", q, sort] as const,
};

/** Results per request. The backend caps a page at 200. */
const PAGE_SIZE = 25;

/**
 * On-device message search, paginated (#1202). With `sort` omitted Rust picks
 * and the page reports which ordering it applied; query filters are parsed in
 * the core.
 */
export function useSearchMessages(query: string, sort: SearchSort | null = null) {
  const trimmed = query.trim();
  return useInfiniteQuery({
    queryKey: searchQueryKeys.messages(trimmed, sort),
    initialPageParam: null as SearchCursor | null,
    queryFn: async ({ pageParam }): Promise<SearchPage> => {
      return await invoke<SearchPage>("search_messages", {
        query: trimmed,
        conversationId: null,
        sort,
        limit: PAGE_SIZE,
        cursor: pageParam,
      });
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    enabled: trimmed.length >= 2,
    staleTime: 1000 * 20,
  });
}
