import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { useObserver } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import * as api from "../../services/api";
import {
  SECURITY_EVENT_FILTERS,
  type SecurityEventFilter,
} from "../../components/Security/securityEvents";

export const securityEventQueryKeys = {
  all: ["security-events"] as const,
  preview: (userId: string | null) => ["security-events", "preview", userId] as const,
  pages: (userId: string | null, filter: SecurityEventFilter) =>
    ["security-events", "pages", userId, filter] as const,
};

/** Rows on the Security page before "View all" (#1548). */
export const SECURITY_EVENTS_PREVIEW = 3;

/** Rows per page on the Security events page (#1548). */
const PAGE_SIZE = 25;

/** The newest few events, asked for exactly. */
export function useSecurityEventsPreview() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: securityEventQueryKeys.preview(currentUser?.id ?? null),
    queryFn: async (): Promise<api.SecurityEvent[]> => {
      if (!currentUser) {
        return [];
      }
      const page = await api.listSecurityEvents(currentUser.id, {
        limit: SECURITY_EVENTS_PREVIEW,
      });
      return page.events;
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}

/** The whole log in keyset pages, filtered (#1548); keeps the previous rows while switching. */
export function useSecurityEventPages(filter: SecurityEventFilter) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useInfiniteQuery({
    queryKey: securityEventQueryKeys.pages(currentUser?.id ?? null, filter),
    initialPageParam: null as api.SecurityEventCursor | null,
    queryFn: async ({ pageParam }): Promise<api.SecurityEventsPage> => {
      if (!currentUser) {
        return { events: [], next_cursor: null };
      }
      return await api.listSecurityEvents(currentUser.id, {
        limit: PAGE_SIZE,
        before: pageParam,
        kinds: SECURITY_EVENT_FILTERS[filter],
      });
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    placeholderData: keepPreviousData,
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}
