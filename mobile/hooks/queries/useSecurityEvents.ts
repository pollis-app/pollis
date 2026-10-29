// Security-event audit trail. Backed by `list_security_events` — enrollments,
// rejections, revocations, identity resets, secret-key rotations (#958) — one
// keyset page at a time, optionally filtered by kind (#1548). Mirrors desktop's
// `hooks/queries/useSecurityEvents.ts`. The `device_revoked` event is the last
// place a revoked device's name is readable (the `user_device` row is deleted
// on revoke), carried as `metadata: "name=<device name>"` (#947).

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

// Rows per page on the Security events screen.
const PAGE_SIZE = 25;

// The newest few events for the Security screen. The DS is asked for exactly
// that many; the full log is its own screen.
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

// The whole log, a keyset page at a time, narrowed by a quick filter.
// `keepPreviousData` holds the previous filter's rows on screen until the new
// filter's first page lands, so a chip tap never blanks the list.
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
