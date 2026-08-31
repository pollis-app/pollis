import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { SearchCursor, SearchPage, SearchSort } from "../../types";

export const searchQueryKeys = {
  search: (query: string, conversationId: string | null, sort: SearchSort | null) =>
    ["search-messages", query, conversationId, sort] as const,
};

/** Results per request. The backend caps a page at 200. */
const PAGE_SIZE = 25;

/**
 * On-device message search (#850).
 *
 * Paginated, because the old hook asked for a hardcoded 50 rows and had no way
 * to ask for more — a search that finds 4,000 messages showed 50 and said
 * nothing about the rest. `total` comes back with every page (`count(*) …
 * MATCH` is ~1.4 ms even for a term in half the corpus), so the UI can say
 * "About N results" honestly.
 *
 * `sort` is optional: omitting it lets Rust pick — relevance for a global
 * search, recency when scoped to one conversation — and the page reports which
 * ordering it actually used.
 */
export function useSearchMessages(
  query: string,
  options: { conversationId?: string | null; sort?: SearchSort | null } = {},
) {
  const trimmed = query.trim();
  const conversationId = options.conversationId ?? null;
  const sort = options.sort ?? null;
  const enabled = trimmed.length >= 2;

  return useInfiniteQuery({
    queryKey: searchQueryKeys.search(trimmed, conversationId, sort),
    initialPageParam: null as SearchCursor | null,
    queryFn: async ({ pageParam }): Promise<SearchPage> => {
      return await invoke<SearchPage>("search_messages", {
        query: trimmed,
        conversationId,
        sort,
        limit: PAGE_SIZE,
        cursor: pageParam,
      });
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor,
    enabled,
    staleTime: 1000 * 30,
  });
}

/**
 * Rebuild the local search index from scratch (#850).
 *
 * The escape hatch for the one drift a contentless FTS5 index can suffer:
 * startup repairs it silently when `integrity-check` catches it, and this is
 * the button for when it does not. Invalidates every cached search so the next
 * one reads the rebuilt index.
 */
export function useRebuildSearchIndex() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async () => {
      await invoke("rebuild_search_index");
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["search-messages"] });
    },
  });
}
