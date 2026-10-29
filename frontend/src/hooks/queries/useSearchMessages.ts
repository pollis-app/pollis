import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { SearchCursor, SearchPage, SearchSort } from "../../types";

export const searchQueryKeys = {
  search: (query: string, conversationId: string | null, sort: SearchSort | null) =>
    ["search-messages", query, conversationId, sort] as const,
};

const PAGE_SIZE = 25;

/**
 * On-device message search (#850), paginated with a `total` per page. Omitting
 * `sort` lets Rust choose (relevance globally, recency in a conversation); the
 * page reports what it used.
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
 * Rebuild the local FTS5 index from scratch (#850), for drift startup's
 * integrity check didn't repair. Invalidates every cached search.
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
