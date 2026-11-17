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
import { PressButton } from "../ui/PressButton";
import { IconButton } from "../ui/IconButton";
import { Kbd } from "../ui/Kbd";
import { FOCUS_RING } from "../ui/focusRing";
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
  // Everything here is free to undo, so every control acts on pointer-down (#1529).
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
                  <PressButton
                    onClick={() => router.navigate({ to: seg.to })}
                    className={`truncate rounded-chip text-dim transition-colors hover:text-accent ${FOCUS_RING}`}
                  >
                    {seg.label}
                  </PressButton>
                )}
              </React.Fragment>
            );
          })}
        </nav>
        <div className="flex items-center gap-3">
          <PressButton
            data-testid="breadcrumb-search-button"
            onClick={openSearch}
            aria-label={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            title={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            className={`flex items-center gap-1.5 rounded-control text-dim transition-colors hover:text-accent ${FOCUS_RING}`}
          >
            <SearchIcon size={16} />
            <Kbd>{searchLabel}</Kbd>
          </PressButton>
          <IconButton
            data-testid="breadcrumb-settings-button"
            activateOn="press"
            onClick={goSettings}
            aria-label={t("breadcrumb.settings")}
            className={isOnSettingsHub ? "text-accent" : "text-dim"}
          >
            <SettingsIcon size={18} />
          </IconButton>
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
              <PressButton
                onClick={() => router.navigate({ to: seg.to })}
                className={`font-mono rounded-chip transition-colors text-inherit hover:text-accent ${FOCUS_RING}`}
              >
                {seg.label}
              </PressButton>
            )}
          </React.Fragment>
        ))}
      </span>
      <PressButton
        data-testid="breadcrumb-search-button"
        onClick={openSearch}
        aria-label={t("breadcrumb.searchButton", { shortcut: searchLabel })}
        title={t("breadcrumb.searchButton", { shortcut: searchLabel })}
        className={`flex h-5 items-center gap-1.5 rounded-control px-1.5 transition-colors text-fg hover:text-accent ${FOCUS_RING}`}
      >
        <SearchIcon size={16} />
        <Kbd>{searchLabel}</Kbd>
      </PressButton>
      <IconButton
        data-testid="breadcrumb-settings-button"
        activateOn="press"
        onClick={goSettings}
        aria-label={t("breadcrumb.settings")}
        className={isOnSettingsHub ? "text-accent" : "text-fg"}
      >
        <SettingsIcon size={16} />
      </IconButton>
    </div>
  );
});
