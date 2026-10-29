import { useInfiniteQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { SearchCursor, SearchPage, SearchSort } from "../../types";

export const searchQueryKeys = {
  search: (query: string, conversationId: string | null, sort: SearchSort | null) =>
    ["search-messages", query, conversationId, sort] as const,
};

/** Results per request. The backend caps a page at 200. */
const PAGE_SIZE = 25;

/** On-device search (#850), paginated with a `total`; omit `sort` to let Rust choose. */
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

/** Rebuild the local FTS index (#850) and invalidate cached searches. */
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
