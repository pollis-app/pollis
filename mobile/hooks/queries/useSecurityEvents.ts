// The security-event audit trail (#958, #1548). Mirrors desktop's
// `hooks/queries/useSecurityEvents.ts`.

import { keepPreviousData, useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import {
  securityEventsArgs,
  type SecurityEvent,
  type SecurityEventCursor,
  type SecurityEventFilter,
  type SecurityEventsPage,
} from "../../lib/securityEvents";

export type { SecurityEvent, SecurityEventFilter } from "../../lib/securityEvents";

export const securityEventQueryKeys = {
  all: ["securityEvents"] as const,
  preview: (userId: string | null) => ["securityEvents", "preview", userId] as const,
  pages: (userId: string | null, filter: SecurityEventFilter) =>
    ["securityEvents", "pages", userId, filter] as const,
};

// Rows on the Security screen before "View all".
export const SECURITY_EVENTS_PREVIEW = 3;

const PAGE_SIZE = 25;

export function useSecurityEventsPreview() {
  const currentUser = useObserver(() => appStore.currentUser);
  return useQuery({
    queryKey: securityEventQueryKeys.preview(currentUser?.id ?? null),
    queryFn: async (): Promise<SecurityEvent[]> => {
      if (!currentUser) {
        return [];
      }
      const page = await invoke<SecurityEventsPage>(
        "list_security_events",
        securityEventsArgs(currentUser.id, SECURITY_EVENTS_PREVIEW, "all", null),
      );
      return page.events;
    },
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}

// `keepPreviousData` holds the previous filter's rows until the new first page
// lands, so a chip tap never blanks the list.
export function useSecurityEventPages(filter: SecurityEventFilter) {
  const currentUser = useObserver(() => appStore.currentUser);
  return useInfiniteQuery({
    queryKey: securityEventQueryKeys.pages(currentUser?.id ?? null, filter),
    initialPageParam: null as SecurityEventCursor | null,
    queryFn: async ({ pageParam }): Promise<SecurityEventsPage> => {
      if (!currentUser) {
        return { events: [], next_cursor: null };
      }
      return await invoke<SecurityEventsPage>(
        "list_security_events",
        securityEventsArgs(currentUser.id, PAGE_SIZE, filter, pageParam),
      );
    },
    getNextPageParam: (lastPage) => lastPage.next_cursor ?? undefined,
    placeholderData: keepPreviousData,
    enabled: !!currentUser,
    staleTime: 1000 * 30,
  });
}
