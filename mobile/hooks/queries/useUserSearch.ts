// Username/email search hook for DM creation. Wraps the
// `search_user_by_username` command — the DS matches an identifier that
// contains `@` against emails ONLY and anything else against usernames ONLY
// (exact, case-sensitive; see `directory::user_by_identifier`). Returns
// `null` when nothing matches; the create-DM screen shows an empty-state row
// in that case.

import { useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";

export interface FoundUser {
  id: string;
  email?: string;
  username: string;
  preferred_name?: string;
  phone?: string;
  avatar_url?: string;
}

export const userSearchQueryKeys = {
  search: (q: string) => ["user-search", q] as const,
};

export function useUserSearch(query: string) {
  const trimmed = query.trim();
  return useQuery({
    queryKey: userSearchQueryKeys.search(trimmed),
    queryFn: async (): Promise<FoundUser | null> => {
      if (!trimmed) {
        return null;
      }
      return await invoke<FoundUser | null>("search_user_by_username", {
        username: trimmed,
      });
    },
    enabled: trimmed.length >= 2,
    staleTime: 1000 * 30,
  });
}
