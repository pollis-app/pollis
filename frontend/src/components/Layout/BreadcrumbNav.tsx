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
import { appStore } from "../../stores/appStore";
import { historyNavStore } from "../../stores/historyNavStore";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useSkin } from "../../hooks/queries/usePreferences";

interface Segment {
  label: string;
  to: string;
}

/**
 * Derives the breadcrumb trail (`Segment[]`) from the current pathname plus the
 * loaded groups / channels / DM data. Pure pathname pattern-matching lifted out
 * of the component body so the render stays readable. Must be called from an
 * `observer()` component — it reads the MobX `appStore.channels` observable.
 */
function useBreadcrumbTrail(): Segment[] {
  const { t } = useTranslation("nav");
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const channels = appStore.channels;

  return useMemo<Segment[]>(() => {
    const out: Segment[] = [{ label: t("breadcrumb.home"), to: "/" }];

    if (pathname === "/") {
      return out;
    }

    if (pathname.startsWith("/groups")) {
      out.push({ label: t("breadcrumb.groups"), to: "/groups" });

      const groupIdMatch = pathname.match(/^\/groups\/([^/]+)/);
      const groupId = groupIdMatch?.[1];

      if (groupId === "new") {
        out.push({ label: t("breadcrumb.createGroup"), to: "/groups/new" });
      } else if (groupId === "search") {
        out.push({ label: t("breadcrumb.findGroup"), to: "/groups/search" });
      } else if (groupId) {
        const group = groupsWithChannels?.find((g) => g.id === groupId);
        if (group) {
          out.push({ label: group.name, to: `/groups/${groupId}` });
        }

        if (pathname.includes("/channels/")) {
          const channelIdMatch = pathname.match(/\/channels\/([^/]+)/);
          const channelId = channelIdMatch?.[1];
          if (channelId === "new") {
            out.push({ label: t("breadcrumb.newChannel"), to: `/groups/${groupId}/channels/new` });
          } else if (channelId) {
            const groupChannels = channels[groupId] ?? [];
            const ch = groupChannels.find((c) => c.id === channelId);
            if (ch) {
              out.push({ label: ch.name, to: `/groups/${groupId}/channels/${channelId}` });
            }
          }
        } else if (pathname.includes("/voice/")) {
          const channelIdMatch = pathname.match(/\/voice\/([^/]+)/);
          const channelId = channelIdMatch?.[1];
          const ch = group?.channels.find((c) => c.id === channelId);
          out.push({
            label: ch?.name ?? t("breadcrumb.voice"),
            to: `/groups/${groupId}/voice/${channelId}`,
          });
        } else if (pathname.endsWith("/join-requests")) {
          out.push({ label: t("breadcrumb.joinRequests"), to: `/groups/${groupId}/join-requests` });
        } else if (pathname.endsWith("/invite")) {
          out.push({ label: t("breadcrumb.inviteMember"), to: `/groups/${groupId}/invite` });
        } else if (pathname.endsWith("/leave")) {
          out.push({ label: t("breadcrumb.leaveGroup"), to: `/groups/${groupId}/leave` });
        } else if (pathname.endsWith("/members")) {
          out.push({ label: t("breadcrumb.members"), to: `/groups/${groupId}/members` });
        } else if (pathname.includes("/members/") && pathname.endsWith("/kick")) {
          out.push({ label: t("breadcrumb.members"), to: `/groups/${groupId}/members` });
          out.push({ label: t("breadcrumb.removeMember"), to: pathname });
        }
      }
    } else if (pathname.startsWith("/dms")) {
      out.push({ label: t("breadcrumb.directMessages"), to: "/dms" });

      const convIdMatch = pathname.match(/^\/dms\/([^/]+)/);
      const conversationId = convIdMatch?.[1];

      if (conversationId === "new") {
        out.push({ label: t("breadcrumb.newMessage"), to: "/dms/new" });
      } else if (conversationId === "requests") {
        out.push({ label: t("breadcrumb.requests"), to: "/dms/requests" });
      } else if (conversationId === "blocked") {
        out.push({ label: t("breadcrumb.blockedUsers"), to: "/dms/blocked" });
      } else if (conversationId) {
        const conv = dmConversations.find((c) => c.id === conversationId);
        if (conv) {
          out.push({
            label: `@${conv.user2_identifier}`,
            to: `/dms/${conversationId}`,
          });
        }
        if (pathname.endsWith("/settings")) {
          out.push({
            label: t("breadcrumb.conversationSettings"),
            to: `/dms/${conversationId}/settings`,
          });
        }
      }
    } else if (pathname === "/settings") {
      out.push({ label: t("breadcrumb.account"), to: "/settings" });
    } else if (pathname === "/preferences") {
      out.push({ label: t("breadcrumb.account"), to: "/settings" });
      out.push({ label: t("breadcrumb.preferences"), to: "/preferences" });
    } else if (pathname === "/user") {
      out.push({ label: t("breadcrumb.account"), to: "/settings" });
      out.push({ label: t("breadcrumb.userSettings"), to: "/user" });
    } else if (pathname.startsWith("/user/")) {
      out.push({ label: t("breadcrumb.profile"), to: pathname });
    } else if (pathname === "/security") {
      out.push({ label: t("breadcrumb.account"), to: "/settings" });
      out.push({ label: t("breadcrumb.security"), to: "/security" });
    } else if (pathname === "/voice-settings") {
      out.push({ label: t("breadcrumb.account"), to: "/settings" });
      out.push({ label: t("breadcrumb.voiceSettings"), to: "/voice-settings" });
    } else if (pathname === "/invites") {
      out.push({ label: t("breadcrumb.invites"), to: "/invites" });
    } else if (pathname === "/join" || pathname.startsWith("/invite/")) {
      out.push({ label: t("breadcrumb.joinAGroup"), to: "/join" });
    } else if (pathname === "/join-requests") {
      out.push({ label: t("breadcrumb.joinRequests"), to: "/join-requests" });
    } else if (pathname === "/search") {
      out.push({ label: t("breadcrumb.search"), to: "/search" });
    }

    return out;
  }, [pathname, groupsWithChannels, dmConversations, channels, t]);
}

interface HistoryNavProps {
  iconSize: number;
}

/**
 * The back/forward chevron pair, the way a browser — and Slack and Discord —
 * draw it: two buttons that walk the router's history, always on screen, each
 * greyed out and genuinely `disabled` when that direction has nowhere to go.
 *
 * Always-visible matters more than it looks: a control that disappears at the
 * root moves everything next to it sideways, so the trail would shift under
 * the pointer on the way home. Disabled keeps the geometry still AND tells a
 * screen reader the same thing the grey tells everyone else, which the
 * `aria-hidden` spacer div it replaced did not.
 *
 * Enablement comes from `historyNavStore`, never from `window.history.length`
 * — see `utils/historyNav.ts` for why that number cannot answer "forward?".
 */
const HistoryNav: React.FC<HistoryNavProps> = observer(({ iconSize }) => {
  const { t } = useTranslation("nav");

  return (
    <div className="flex flex-shrink-0 items-center gap-0.5">
      <Button
        variant="ghost"
        size="xs"
        data-testid="breadcrumb-back-button"
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
        onClick={historyNavStore.goForward}
        disabled={!historyNavStore.canGoForward}
        aria-label={t("common:actions.forward")}
      >
        <ChevronRight size={iconSize} className="rtl-mirror" />
      </Button>
    </div>
  );
});

/**
 * BreadcrumbNav renders below the TitleBar on authenticated pages.
 * Shows the browser-style back/forward pair (router history, not the
 * breadcrumb hierarchy) plus the breadcrumb trail itself
 * ("Home / Direct Messages / @someone"), whose segments stay clickable for
 * the hierarchical jumps the arrows no longer do.
 */
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

  const isOnSettingsHub = pathname === "/settings";

  if (skin === "refined") {
    return (
      <div
        data-testid="breadcrumb-nav"
        className="h-[2.75rem] flex-shrink-0 flex items-center gap-3 border-b border-line bg-surface px-3"
      >
        <HistoryNav iconSize={18} />
        <nav
          data-testid="breadcrumb-trail"
          className="flex min-w-0 flex-1 items-center gap-1.5 text-sm"
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
                    onClick={() => router.navigate({ to: seg.to })}
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
            onClick={openSearch}
            aria-label={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            title={t("breadcrumb.searchButton", { shortcut: searchLabel })}
            className="flex items-center gap-1.5 text-dim transition-colors hover:text-accent"
          >
            <SearchIcon size={16} />
            <kbd
              aria-hidden="true"
              className="font-mono font-machine rounded-control border border-line bg-bg px-1.5 py-0.5 text-xs leading-none text-muted"
            >
              {searchLabel}
            </kbd>
          </button>
          <button
            data-testid="breadcrumb-settings-button"
            onClick={() => router.navigate({ to: "/settings" })}
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
      className="border-b border-line bg-surface"
      style={{
        height: 28,
        flexShrink: 0,
        display: "flex",
        alignItems: "center",
        gap: 8,
        paddingInlineStart: 8,
        paddingInlineEnd: 12,
      }}
    >
      <HistoryNav iconSize={14} />
      <span
        data-testid="breadcrumb-trail"
        className="text-xs font-mono truncate text-muted"
        style={{ flex: 1 }}
      >
        {segments.map((seg, i) => (
          <React.Fragment key={`${seg.to}-${i}`}>
            {i > 0 && <span style={{ opacity: 0.5 }}> / </span>}
            {i === segments.length - 1 ? (
              <bdi className="text-fg">{seg.label}</bdi>
            ) : (
              <button
                onClick={() => router.navigate({ to: seg.to })}
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
        onClick={openSearch}
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
          className="font-mono font-machine text-xs bg-bg border border-line"
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
        onClick={() => router.navigate({ to: "/settings" })}
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
