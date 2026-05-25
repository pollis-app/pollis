import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import type { SearchResult } from "../../types";

export const searchQueryKeys = {
  search: (query: string) => ["search-messages", query] as const,
};

export function useSearchMessages(query: string) {
  const trimmed = query.trim();
  const enabled = trimmed.length >= 2;

  return useQuery({
    queryKey: searchQueryKeys.search(trimmed),
    queryFn: async (): Promise<SearchResult[]> => {
      const results = await invoke<SearchResult[]>("search_messages", {
        query: trimmed,
        limit: 50,
      });
      return results || [];
    },
    enabled,
    staleTime: 1000 * 30,
  });
}
