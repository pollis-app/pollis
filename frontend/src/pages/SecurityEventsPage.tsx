import React, { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { SecurityEventRow } from "../components/Security/SecurityEventRow";
import {
  SECURITY_EVENT_FILTER_ORDER,
  type SecurityEventFilter,
} from "../components/Security/securityEvents";
import { useSecurityEventPages } from "../hooks/queries/useSecurityEvents";
import { errorMessage } from "../utils/errorMessage";

const FILTER_LABEL_KEYS: Record<SecurityEventFilter, string> = {
  all: "security.eventsFilterAll",
  devices: "security.eventsFilterDevices",
  identity: "security.eventsFilterIdentity",
  account: "security.eventsFilterAccount",
};

/**
 * The whole security-event log (#1548): keyset pages from the DS, more loaded
 * as the end of the list scrolls into view, narrowed by quick-filter chips.
 * The Security page shows only the newest three and links here.
 */
export const SecurityEventsPage: React.FC = () => {
  const { t } = useTranslation("settings");
  const [filter, setFilter] = useState<SecurityEventFilter>("all");
  const {
    data,
    isPending,
    isError,
    error,
    isPlaceholderData,
    hasNextPage,
    fetchNextPage,
    isFetchingNextPage,
  } = useSecurityEventPages(filter);

  const events = useMemo(() => data?.pages.flatMap((p) => p.events) ?? [], [data]);

  // Infinite scroll: the sentinel under the list asks for the next page as it
  // nears the viewport. The button beside it is the same action for keyboard
  // and assistive-tech users, and for a list too short to scroll.
  const sentinelRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    const el = sentinelRef.current;
    if (!el || !hasNextPage || isPlaceholderData) {
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting) && !isFetchingNextPage) {
          void fetchNextPage();
        }
      },
      { rootMargin: "200px" },
    );
    observer.observe(el);
    return () => observer.disconnect();
  }, [hasNextPage, isFetchingNextPage, isPlaceholderData, fetchNextPage]);

  return (
    <PageShell title={t("security.eventsHeading")} scrollable>
      <div className="flex justify-center px-6 py-8">
        <div
          className="flex flex-col gap-4 w-full max-w-md font-mono"
          data-testid="security-events-page"
        >
          <p className="text-xs text-muted" style={{ lineHeight: 1.5 }}>
            {t("security.eventsDescription")}
          </p>

          <div
            role="group"
            aria-label={t("security.eventsFilterAria")}
            className="flex flex-wrap gap-2"
          >
            {SECURITY_EVENT_FILTER_ORDER.map((f) => (
              <Button
                key={f}
                data-testid={`security-events-filter-${f}`}
                variant={filter === f ? "primary" : "secondary"}
                size="xs"
                aria-pressed={filter === f}
                onClick={() => setFilter(f)}
              >
                {t(FILTER_LABEL_KEYS[f])}
              </Button>
            ))}
          </div>

          {isError && (
            <p data-testid="security-events-error" className="text-xs text-danger">
              {errorMessage(error, t("security.eventsLoadFailed"))}
            </p>
          )}

          {isPending ? (
            <p className="text-xs font-mono text-muted">{t("common:states.loading")}</p>
          ) : events.length === 0 && !isPlaceholderData ? (
            <p data-testid="security-events-empty" className="text-xs font-mono text-dim">
              {filter === "all" ? t("security.eventsEmpty") : t("security.eventsEmptyFiltered")}
            </p>
          ) : (
            // The previous filter's rows stay up, dimmed, until the new
            // filter's first page lands — feedback on the same frame as the
            // tap, and no flash of an empty list.
            <div
              data-testid="security-events-list"
              data-filter={filter}
              data-stale={isPlaceholderData ? "true" : "false"}
              aria-busy={isPlaceholderData}
              className={`flex flex-col transition-opacity ${isPlaceholderData ? "opacity-50" : ""}`}
            >
              {events.map((event) => (
                <SecurityEventRow key={event.id} event={event} />
              ))}
            </div>
          )}

          {hasNextPage && !isPlaceholderData && (
            <div ref={sentinelRef} className="px-4 pt-2">
              <Button
                data-testid="security-events-load-more"
                variant="ghost"
                size="sm"
                isLoading={isFetchingNextPage}
                loadingText={t("common:states.loading")}
                disabled={isFetchingNextPage}
                onClick={() => void fetchNextPage()}
              >
                {t("security.eventsLoadMore")}
              </Button>
            </div>
          )}
        </div>
      </div>
    </PageShell>
  );
};
