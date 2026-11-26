// The share picker's rows (#2100): pollis-core `share_targets`, the same query and order the iOS
// share extension lists. Never a DM request, a blocked DM or a voice channel.

import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";

export interface ShareTarget {
  conversation_id: string;
  kind: "dm" | "channel" | "vault";
  title: string;
  subtitle: string | null;
  avatar_user_id: string | null;
  last_activity_at: string | null;
  group_id: string | null;
}

const shareTargetQueryKeys = {
  all: ["shareTargets"] as const,
  search: (userId: string | null, query: string) => ["shareTargets", userId, query] as const,
};

/** Rows matching `query` (all when blank), best first. The previous rows stay while typing. */
export function useShareTargets(query: string) {
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const q = query.trim();
  return useQuery({
    queryKey: shareTargetQueryKeys.search(userId, q),
    queryFn: () => invoke<ShareTarget[]>("share_targets", { query: q || null, limit: 50 }),
    enabled: !!userId,
    placeholderData: keepPreviousData,
  });
}
