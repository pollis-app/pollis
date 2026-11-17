import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useRouter, useRouterState } from "@tanstack/react-router";
import {
  ChevronLeft,
  ChevronRight,
  Search as SearchIcon,
  Settings as SettingsIcon,
} from "lucide-react";
import { observer } from "mobx-react-lite";
import { useShortcutLabel } from "../../keyboard";
import { Button } from "../ui/Button";
import { usePressActivator } from "../../hooks/usePressActivation";
import { appStore } from "../../stores/appStore";
import { historyNavStore } from "../../stores/historyNavStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useSkin } from "../../hooks/queries/usePreferences";
import { breadcrumbTrail, type Segment } from "../../utils/breadcrumbTrail";


/** Breadcrumb trail from the pathname; call from an `observer()` (reads `appStore.channels`). */
function useBreadcrumbTrail(): Segment[] {
  const { t } = useTranslation("nav");
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const channels = appStore.channels;

  return useMemo<Segment[]>(
    () => breadcrumbTrail(pathname, { groups: groupsWithChannels, channels, dms: dmConversations }, t),
    [pathname, groupsWithChannels, dmConversations, channels, t],
  );
}

interface HistoryNavProps {
  iconSize: number;
}

/** Back/forward over router history, always shown and `disabled` when empty; state from `historyNavStore` (see utils/historyNav.ts). */
const HistoryNav: React.FC<HistoryNavProps> = observer(({ iconSize }) => {
  const { t } = useTranslation("nav");

  return (
    <div className="flex flex-shrink-0 items-center gap-0.5">
      <Button
        variant="ghost"
        size="xs"
        data-testid="breadcrumb-back-button"
        activateOn="press"
        onClick={historyNavStore.goBack}
        disabled={!historyNavStore.canGoBack}
        aria-label={t("common:actions.back")}
      >
        <ChevronLeft size={iconSize} className="rtl-mirror" />
      </Button>
      <Button
        variant="ghost"
        size="xs"
        data-testid="breadcrumb-forward-button"
        activateOn="press"
        onClick={historyNavStore.goForward}
        disabled={!historyNavStore.canGoForward}
        aria-label={t("common:actions.forward")}
      >
        <ChevronRight size={iconSize} className="rtl-mirror" />
      </Button>
    </div>
  );
});

/** History chevrons plus the clickable breadcrumb trail. */
export const BreadcrumbNav: React.FC = observer(() => {
  const { t } = useTranslation("nav");
  const router = useRouter();
  const skin = useSkin();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const searchLabel = useShortcutLabel("app.toggleSearch");
  const segments = useBreadcrumbTrail();

  const openSearch = () => {
    window.dispatchEvent(new CustomEvent("pollis:open-search"));
  };
  // Everything here is free to undo, so pointer-down (#1529).
  const press = usePressActivator();
  const goSettings = () => router.navigate({ to: "/settings" });

  const isOnSettingsHub = pathname === "/settings";

  if (skin === "modern") {
    return (
      <div
        data-testid="breadcrumb-nav"
        className="h-bar flex-shrink-0 flex items-center gap-3 border-b border-line bg-surface px-3"
      >
        <HistoryNav iconSize={18} />
        <nav
          data-testid="breadcrumb-trail"
          className="flex min-w-0 flex-1 items-center gap-1.5 text-body"
        >
          {segments.map((seg, i) => {
            const isLast = i === segments.length - 1;
            return (
              <React.Fragment key={`${seg.to}-${i}`}>
                {i > 0 && (
                  <span className="select-none text-muted" aria-hidden="true">
                    /
                  </span>
                )}
                {isLast ? (
                  <bdi className="truncate font-semibold text-accent">{seg.label}</bdi>
                ) : (
                  <button
                    {...press(() => router.navigate({ to: seg.to }), "press")}
                    className="truncate text-dim transition-colors hover:text-accent"
                  >
                    {seg.label}
                  </button>
                )}
              </React.Fragment>
            );
          })}
        </nav>
        <div className="flex items-center gap-3">
          <button
            data-testid="breadcrumb-search-button"
            {...press(openSearch, "press")}
            aria-label={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            title={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            className="flex items-center gap-1.5 text-dim transition-colors hover:text-accent"
          >
            <SearchIcon size={16} />
            <kbd
              aria-hidden="true"
              className="font-mono font-machine rounded-control border border-line bg-bg px-1.5 py-0.5 text-secondary leading-none text-muted"
            >
              {searchLabel}
            </kbd>
          </button>
          <button
            data-testid="breadcrumb-settings-button"
            {...press(goSettings, "press")}
            aria-label={t("breadcrumb.settings")}
            className={`flex items-center justify-center rounded-control p-1 hover:bg-hover hover:text-accent ${isOnSettingsHub ? "text-accent" : "text-dim"}`}
          >
            <SettingsIcon size={18} />
          </button>
        </div>
      </div>
    );
  }

  return (
    <div
      data-testid="breadcrumb-nav"
      // `h-bar` in both skins (#1966), the height every chrome bar shares.
      className="h-bar flex-shrink-0 flex items-center gap-2 border-b border-line bg-surface ps-2 pe-3"
    >
      <HistoryNav iconSize={14} />
      <span
        data-testid="breadcrumb-trail"
        className="text-secondary font-mono truncate text-muted"
        style={{ flex: 1 }}
      >
        {segments.map((seg, i) => (
          <React.Fragment key={`${seg.to}-${i}`}>
            {i > 0 && <span style={{ opacity: 0.5 }}> / </span>}
            {i === segments.length - 1 ? (
              <bdi className="text-fg">{seg.label}</bdi>
            ) : (
              <button
                {...press(() => router.navigate({ to: seg.to }), "press")}
                className="font-mono transition-colors text-inherit hover:text-accent border-0"
                style={{
                  background: "none",
                  padding: 0,
                  cursor: "pointer",
                  fontSize: "inherit",
                }}
              >
                {seg.label}
              </button>
            )}
          </React.Fragment>
        ))}
      </span>
      <button
        data-testid="breadcrumb-search-button"
        {...press(openSearch, "press")}
        aria-label={t("breadcrumb.searchButton", { shortcut: searchLabel })}
        title={t("breadcrumb.searchButton", { shortcut: searchLabel })}
        className="flex items-center gap-1.5 transition-colors text-fg hover:text-accent border-0"
        style={{
          height: 20,
          background: "none",
          padding: "0 6px",
          cursor: "pointer",
        }}
      >
        <SearchIcon size={16} />
        <kbd
          aria-hidden="true"
          className="font-mono font-machine text-secondary bg-bg border border-line"
          style={{
            color: "inherit",
            padding: "1px 5px",
            borderRadius: 3,
            lineHeight: 1.2,
          }}
        >
          {searchLabel}
        </kbd>
      </button>
      <button
        data-testid="breadcrumb-settings-button"
        {...press(goSettings, "press")}
        aria-label={t("breadcrumb.settings")}
        className={`flex items-center justify-center bg-transparent hover:bg-hover hover:text-accent border-0 ${isOnSettingsHub ? "text-accent" : "text-fg"}`}
        style={{
          width: 24,
          height: 24,
          padding: 0,
          borderRadius: 4,
          cursor: "pointer",
        }}
      >
        <SettingsIcon size={16} />
      </button>
    </div>
  );
});
