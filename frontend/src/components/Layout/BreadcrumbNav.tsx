import React, { useMemo } from "react";
import { useRouter, useRouterState } from "@tanstack/react-router";
import { ChevronLeft, Search as SearchIcon, Settings as SettingsIcon } from "lucide-react";
import { observer } from "mobx-react-lite";
import { useShortcutLabel } from "../../keyboard";
import { appStore } from "../../stores/appStore";
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
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const channels = appStore.channels;

  return useMemo<Segment[]>(() => {
    const out: Segment[] = [{ label: "Home", to: "/" }];

    if (pathname === "/") {
      return out;
    }

    if (pathname.startsWith("/groups")) {
      out.push({ label: "Groups", to: "/groups" });

      const groupIdMatch = pathname.match(/^\/groups\/([^/]+)/);
      const groupId = groupIdMatch?.[1];

      if (groupId === "new") {
        out.push({ label: "Create Group", to: "/groups/new" });
      } else if (groupId === "search") {
        out.push({ label: "Find Group", to: "/groups/search" });
      } else if (groupId) {
        const group = groupsWithChannels?.find((g) => g.id === groupId);
        if (group) {
          out.push({ label: group.name, to: `/groups/${groupId}` });
        }

        if (pathname.includes("/channels/")) {
          const channelIdMatch = pathname.match(/\/channels\/([^/]+)/);
          const channelId = channelIdMatch?.[1];
          if (channelId === "new") {
            out.push({ label: "New Channel", to: `/groups/${groupId}/channels/new` });
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
            label: ch?.name ?? "voice",
            to: `/groups/${groupId}/voice/${channelId}`,
          });
        } else if (pathname.endsWith("/join-requests")) {
          out.push({ label: "Join Requests", to: `/groups/${groupId}/join-requests` });
        } else if (pathname.endsWith("/invite")) {
          out.push({ label: "Invite Member", to: `/groups/${groupId}/invite` });
        } else if (pathname.endsWith("/leave")) {
          out.push({ label: "Leave Group", to: `/groups/${groupId}/leave` });
        } else if (pathname.endsWith("/members")) {
          out.push({ label: "Members", to: `/groups/${groupId}/members` });
        } else if (pathname.includes("/members/") && pathname.endsWith("/kick")) {
          out.push({ label: "Members", to: `/groups/${groupId}/members` });
          out.push({ label: "Remove Member", to: pathname });
        }
      }
    } else if (pathname.startsWith("/dms")) {
      out.push({ label: "Direct Messages", to: "/dms" });

      const convIdMatch = pathname.match(/^\/dms\/([^/]+)/);
      const conversationId = convIdMatch?.[1];

      if (conversationId === "new") {
        out.push({ label: "New Message", to: "/dms/new" });
      } else if (conversationId === "requests") {
        out.push({ label: "Requests", to: "/dms/requests" });
      } else if (conversationId === "blocked") {
        out.push({ label: "Blocked Users", to: "/dms/blocked" });
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
            label: "Conversation Settings",
            to: `/dms/${conversationId}/settings`,
          });
        }
      }
    } else if (pathname === "/settings") {
      out.push({ label: "Account", to: "/settings" });
    } else if (pathname === "/preferences") {
      out.push({ label: "Account", to: "/settings" });
      out.push({ label: "Preferences", to: "/preferences" });
    } else if (pathname === "/user") {
      out.push({ label: "Account", to: "/settings" });
      out.push({ label: "User Settings", to: "/user" });
    } else if (pathname.startsWith("/user/")) {
      out.push({ label: "Profile", to: pathname });
    } else if (pathname === "/security") {
      out.push({ label: "Account", to: "/settings" });
      out.push({ label: "Security", to: "/security" });
    } else if (pathname === "/voice-settings") {
      out.push({ label: "Account", to: "/settings" });
      out.push({ label: "Voice", to: "/voice-settings" });
    } else if (pathname === "/invites") {
      out.push({ label: "Invites", to: "/invites" });
    } else if (pathname === "/join-requests") {
      out.push({ label: "Join Requests", to: "/join-requests" });
    } else if (pathname === "/search") {
      out.push({ label: "Search", to: "/search" });
    }

    return out;
  }, [pathname, groupsWithChannels, dmConversations, channels]);
}

/**
 * BreadcrumbNav renders below the TitleBar on authenticated pages.
 * Shows a persistent back button that navigates up one level in the
 * breadcrumb hierarchy (not browser-history back) plus the breadcrumb
 * trail itself ("Home / Direct Messages / @someone").
 */
export const BreadcrumbNav: React.FC = observer(() => {
  const router = useRouter();
  const skin = useSkin();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const searchLabel = useShortcutLabel("app.toggleSearch");
  const segments = useBreadcrumbTrail();

  // Back = up one segment in the breadcrumb stack (not browser history)
  const parentTo = segments.length > 1 ? segments[segments.length - 2].to : null;

  const handleBack = () => {
    if (!parentTo) {
      return;
    }
    router.navigate({ to: parentTo });
  };

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
        {parentTo ? (
          <button
            data-testid="breadcrumb-back-button"
            onClick={handleBack}
            aria-label="Back"
            className="flex items-center justify-center text-dim transition-colors hover:text-accent"
          >
            <ChevronLeft size={18} />
          </button>
        ) : (
          <div className="w-[18px]" aria-hidden="true" />
        )}
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
                  <span className="truncate font-semibold text-accent">{seg.label}</span>
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
            aria-label={`Search (${searchLabel})`}
            title={`Search (${searchLabel})`}
            className="flex items-center gap-1.5 text-dim transition-colors hover:text-accent"
          >
            <SearchIcon size={16} />
            <kbd
              aria-hidden="true"
              className="font-mono font-machine rounded-[var(--radius-control)] border border-line bg-bg px-1.5 py-0.5 text-xs leading-none text-muted"
            >
              {searchLabel}
            </kbd>
          </button>
          <button
            data-testid="breadcrumb-settings-button"
            onClick={() => router.navigate({ to: "/settings" })}
            aria-label="Settings"
            className={`flex items-center justify-center rounded-[var(--radius-control)] p-1 transition-colors hover:bg-hover hover:text-accent ${isOnSettingsHub ? "text-accent" : "text-dim"}`}
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
      style={{
        height: 28,
        flexShrink: 0,
        borderBottom: "1px solid var(--c-border)",
        background: "var(--c-surface)",
        display: "flex",
        alignItems: "center",
        gap: 8,
        paddingLeft: 8,
        paddingRight: 12,
      }}
    >
      {parentTo ? (
        <button
          data-testid="breadcrumb-back-button"
          onClick={handleBack}
          aria-label="Back"
          className="flex items-center justify-center transition-colors text-[var(--c-text-muted)] hover:text-[var(--c-accent)]"
          style={{
            width: 20,
            height: 20,
            background: "none",
            border: "none",
            padding: 0,
            cursor: "pointer",
          }}
        >
          <ChevronLeft size={14} />
        </button>
      ) : (
        <div style={{ width: 20, height: 20 }} aria-hidden="true" />
      )}
      <span
        data-testid="breadcrumb-trail"
        className="text-xs font-mono truncate"
        style={{ color: "var(--c-text-muted)", flex: 1 }}
      >
        {segments.map((seg, i) => (
          <React.Fragment key={`${seg.to}-${i}`}>
            {i > 0 && <span style={{ opacity: 0.5 }}> / </span>}
            {i === segments.length - 1 ? (
              <span style={{ color: "var(--c-text)" }}>{seg.label}</span>
            ) : (
              <button
                onClick={() => router.navigate({ to: seg.to })}
                className="font-mono transition-colors text-inherit hover:text-[var(--c-accent)]"
                style={{
                  background: "none",
                  border: "none",
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
        aria-label={`Search (${searchLabel})`}
        title={`Search (${searchLabel})`}
        className="flex items-center gap-1.5 transition-colors text-[var(--c-text)] hover:text-[var(--c-accent)]"
        style={{
          height: 20,
          background: "none",
          border: "none",
          padding: "0 6px",
          cursor: "pointer",
        }}
      >
        <SearchIcon size={16} />
        <kbd
          aria-hidden="true"
          className="font-mono font-machine text-xs"
          style={{
            color: "inherit",
            background: "var(--c-bg)",
            padding: "1px 5px",
            borderRadius: 3,
            border: "1px solid var(--c-border)",
            lineHeight: 1.2,
          }}
        >
          {searchLabel}
        </kbd>
      </button>
      <button
        data-testid="breadcrumb-settings-button"
        onClick={() => router.navigate({ to: "/settings" })}
        aria-label="Settings"
        className={`flex items-center justify-center transition-colors bg-transparent hover:bg-[var(--c-hover)] hover:text-[var(--c-accent)] ${isOnSettingsHub ? "text-[var(--c-accent)]" : "text-[var(--c-text)]"}`}
        style={{
          width: 24,
          height: 24,
          border: "none",
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
