import React, { useEffect, useMemo, useState } from "react";
import { useRouter, useRouterState } from "@tanstack/react-router";
import {
  ChevronDown,
  ChevronRight,
  Hash,
  Volume2,
  Settings as SettingsIcon,
  Users,
  MessageCircle,
  Palette,
  User as UserIcon,
  ShieldCheck,
} from "lucide-react";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useAppStore } from "../../stores/appStore";
import { shortcutLabel } from "../../utils/platform";

const SIDEBAR_WIDTH = 220;
const COLLAPSED_GROUPS_KEY = "pollis.sidebar.collapsedGroups";

// The sidebar chrome is sized in rem so it tracks the user's font-size
// preference (`--font-size-base` on :root, which scales rem). Hardcoded px
// would stay frozen while the rest of the app scales. Values are expressed
// relative to the 15px default base; rem keeps them reactive to live
// changes without needing a re-render.
const BASE_FONT_PX = 15;
const rem = (px: number): string => `${px / BASE_FONT_PX}rem`;
// Shared lucide sizing: `size` seeds the SVG attribute, the rem width/height
// override actually scales it with the font preference.
const iconProps = {
  size: 14,
  style: { width: rem(14), height: rem(14), flexShrink: 0 },
} as const;

interface SidebarProps {
  isOpen: boolean;
  onToggle: () => void;
}

export const Sidebar: React.FC<SidebarProps> = ({ isOpen, onToggle }) => {
  const router = useRouter();
  const pathname = useRouterState({ select: (s) => s.location.pathname });

  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  const unreadCounts = useAppStore((s) => s.unreadCounts);

  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(() => {
    try {
      const raw = localStorage.getItem(COLLAPSED_GROUPS_KEY);
      return new Set(raw ? (JSON.parse(raw) as string[]) : []);
    } catch {
      return new Set();
    }
  });
  useEffect(() => {
    try {
      localStorage.setItem(COLLAPSED_GROUPS_KEY, JSON.stringify([...collapsedGroups]));
    } catch {
      /* localStorage unavailable — non-fatal */
    }
  }, [collapsedGroups]);

  const toggleGroup = (id: string) => {
    setCollapsedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  const totalDmUnread = useMemo(
    () => dmConversations.reduce((sum, c) => sum + (unreadCounts[c.id] ?? 0), 0),
    [dmConversations, unreadCounts]
  );

  if (!isOpen) {
    return null;
  }

  const isOnGroups = pathname === "/groups";
  const isOnDms = pathname === "/dms";
  const activeChannelId = (() => {
    const m = pathname.match(/^\/groups\/[^/]+\/channels\/([^/]+)/);
    return m ? m[1] : null;
  })();
  const activeVoiceId = (() => {
    const m = pathname.match(/^\/groups\/[^/]+\/voice\/([^/]+)/);
    return m ? m[1] : null;
  })();
  const activeDmId = (() => {
    const m = pathname.match(/^\/dms\/([^/]+)/);
    return m && m[1] !== "new" && m[1] !== "requests" && m[1] !== "blocked" ? m[1] : null;
  })();
  const activeGroupId = (() => {
    const m = pathname.match(/^\/groups\/([^/]+)/);
    return m && m[1] !== "new" && m[1] !== "search" ? m[1] : null;
  })();

  const isOnSettingsHub = pathname === "/settings";
  const settingsItems = [
    { id: "preferences", label: "Preferences", icon: <Palette {...iconProps} />, to: "/preferences" as const, isActive: pathname === "/preferences" },
    { id: "user", label: "User Settings", icon: <UserIcon {...iconProps} />, to: "/user" as const, isActive: pathname === "/user" },
    { id: "voice-settings", label: "Voice", icon: <Volume2 {...iconProps} />, to: "/voice-settings" as const, isActive: pathname === "/voice-settings" },
    { id: "security", label: "Security", icon: <ShieldCheck {...iconProps} />, to: "/security" as const, isActive: pathname === "/security" || pathname.startsWith("/security/") },
  ];
  const isOnAnySettings = isOnSettingsHub || settingsItems.some((s) => s.isActive);

  return (
    <aside
      data-testid="sidebar"
      style={{
        width: rem(SIDEBAR_WIDTH),
        flexShrink: 0,
        borderRight: "1px solid var(--c-border)",
        background: "var(--c-surface)",
        display: "flex",
        flexDirection: "column",
        fontFamily: "var(--font-mono, monospace)",
      }}
    >
      <div style={{ flex: 1, overflowY: "auto", overflowX: "hidden" }}>
        <SectionHeader
          label="groups"
          icon={<Users {...iconProps} />}
          isActive={isOnGroups}
          onClick={() => router.navigate({ to: "/groups" })}
          borderedBottom
        />

        <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
          {groupsWithChannels.map((group) => {
            const isCollapsed = collapsedGroups.has(group.id);
            const groupUnread = group.channels.reduce(
              (sum, ch) => sum + (unreadCounts[ch.id] ?? 0),
              0
            );
            const isGroupActive = activeGroupId === group.id;
            return (
              <li key={group.id}>
                <Row
                  indent={1}
                  isActive={isGroupActive && !activeChannelId && !activeVoiceId}
                  onClick={() => router.navigate({ to: "/groups/$groupId", params: { groupId: group.id } })}
                  chevron={{
                    isCollapsed,
                    onToggle: () => toggleGroup(group.id),
                    ariaLabel: isCollapsed ? `Expand ${group.name}` : `Collapse ${group.name}`,
                  }}
                  label={group.name}
                  badge={isCollapsed && groupUnread > 0 ? groupUnread : null}
                />
                {!isCollapsed &&
                  group.channels.map((ch) => {
                    const isVoice = ch.channel_type === "voice";
                    const unread = unreadCounts[ch.id] ?? 0;
                    const isActive = isVoice ? activeVoiceId === ch.id : activeChannelId === ch.id;
                    return (
                      <Row
                        key={ch.id}
                        indent={2}
                        isActive={isActive}
                        onClick={() =>
                          isVoice
                            ? router.navigate({
                              to: "/groups/$groupId/voice/$channelId",
                              params: { groupId: group.id, channelId: ch.id },
                            })
                            : router.navigate({
                              to: "/groups/$groupId/channels/$channelId",
                              params: { groupId: group.id, channelId: ch.id },
                            })
                        }
                        leading={isVoice ? <Volume2 {...iconProps} /> : <Hash {...iconProps} />}
                        label={ch.name}
                        badge={unread > 0 ? unread : null}
                      />
                    );
                  })}
              </li>
            );
          })}
        </ul>

        <SectionHeader
          label="dms"
          icon={<MessageCircle {...iconProps} />}
          isActive={isOnDms}
          onClick={() => router.navigate({ to: "/dms" })}
          badge={totalDmUnread > 0 ? totalDmUnread : null}
          bordered
        />

        <ul style={{ margin: 0, padding: 0, listStyle: "none" }}>
          {dmConversations.map((c) => {
            const unread = unreadCounts[c.id] ?? 0;
            return (
              <Row
                key={c.id}
                indent={1}
                isActive={activeDmId === c.id}
                onClick={() => router.navigate({ to: "/dms/$conversationId", params: { conversationId: c.id } })}
                label={`@${c.user2_identifier}`}
                badge={unread > 0 ? unread : null}
              />
            );
          })}
        </ul>

        <SectionHeader
          label="account"
          icon={<SettingsIcon {...iconProps} />}
          isActive={isOnAnySettings}
          onClick={() => router.navigate({ to: "/settings" })}
          bordered
        />
        {settingsItems.map((s) => (
          <Row
            key={s.id}
            indent={1}
            isActive={s.isActive}
            onClick={() => router.navigate({ to: s.to })}
            leading={s.icon}
            label={s.label}
          />
        ))}
      </div>

      <button
        type="button"
        data-testid="sidebar-close"
        onClick={onToggle}
        aria-label={`Close sidebar (${shortcutLabel("B")})`}
        title={`Close sidebar (${shortcutLabel("B")})`}
        style={{
          flexShrink: 0,
          display: "flex",
          alignItems: "center",
          gap: rem(8),
          padding: `${rem(8)} ${rem(10)}`,
          borderTop: "1px solid var(--c-border)",
          background: "none",
          color: "var(--c-text-muted)",
          fontFamily: "inherit",
          fontSize: rem(13),
          textAlign: "left",
          cursor: "pointer",
        }}
        onMouseEnter={(e) => {
          (e.currentTarget as HTMLButtonElement).style.color = "var(--c-text)";
        }}
        onMouseLeave={(e) => {
          (e.currentTarget as HTMLButtonElement).style.color = "var(--c-text-muted)";
        }}
      >
        <span style={{ flex: 1 }}>Close</span>
        <kbd
          aria-hidden="true"
          className="font-mono"
          style={{
            color: "inherit",
            background: "var(--c-bg)",
            padding: `${rem(1)} ${rem(5)}`,
            borderRadius: 3,
            border: "1px solid var(--c-border)",
            fontSize: rem(11),
            lineHeight: 1.2,
          }}
        >
          {shortcutLabel("B")}
        </kbd>
      </button>
    </aside>
  );
};

interface SectionHeaderProps {
  label: string;
  icon: React.ReactNode;
  isActive: boolean;
  onClick: () => void;
  badge?: number | null;
  /** Hairline rules above and below the header. */
  bordered?: boolean;
  /** Hairline rule below the header only. */
  borderedBottom?: boolean;
}

const SectionHeader: React.FC<SectionHeaderProps> = ({ label, icon, isActive, onClick, badge, bordered, borderedBottom }) => (
  <button
    type="button"
    onClick={onClick}
    style={{
      width: "100%",
      display: "flex",
      alignItems: "center",
      gap: rem(6),
      padding: `${rem(8)} ${rem(10)} ${rem(9)}`,
      marginTop: bordered ? rem(4) : 0,
      background: "var(--c-surface)",
      border: "none",
      borderTop: bordered ? "1px solid var(--c-border)" : "none",
      borderBottom: bordered || borderedBottom ? "1px solid var(--c-border)" : "none",
      color: isActive ? "var(--c-accent)" : "var(--c-text-muted)",
      fontSize: rem(12),
      letterSpacing: "0.08em",
      textTransform: "uppercase",
      cursor: "pointer",
      textAlign: "left",
      transition: "background 75ms",
      position: "sticky",
      top: 0,
      zIndex: 1,
    }}
    onMouseEnter={(e) => {
      (e.currentTarget as HTMLButtonElement).style.background = "var(--c-hover)";
    }}
    onMouseLeave={(e) => {
      (e.currentTarget as HTMLButtonElement).style.background = "var(--c-surface)";
    }}
  >
    {icon}
    <span style={{ flex: 1 }}>{label}</span>
    {badge != null && <UnreadBadge count={badge} muted />}
  </button>
);

interface RowChevron {
  isCollapsed: boolean;
  onToggle: () => void;
  ariaLabel: string;
}

interface RowProps {
  indent: number;
  isActive: boolean;
  onClick: () => void;
  leading?: React.ReactNode;
  /** When provided, renders an expand/collapse chevron as a sibling button outside the navigating button. */
  chevron?: RowChevron;
  label: string;
  badge?: number | null;
}

const Row: React.FC<RowProps> = ({ indent, isActive, onClick, leading, chevron, label, badge }) => {
  const setHover = (el: HTMLElement, on: boolean) => {
    if (isActive) {
      return;
    }
    el.style.background = on ? "var(--c-hover)" : "none";
  };
  return (
    <div
      data-active={isActive ? "true" : "false"}
      style={{
        display: "flex",
        alignItems: "stretch",
        width: "100%",
        background: isActive ? "var(--c-hover)" : "none",
        borderLeft: isActive ? "2px solid var(--c-accent)" : "2px solid transparent",
        color: isActive ? "var(--c-accent)" : "var(--c-text)",
      }}
      onMouseEnter={(e) => setHover(e.currentTarget, true)}
      onMouseLeave={(e) => setHover(e.currentTarget, false)}
    >
      {chevron && (
        <button
          type="button"
          tabIndex={-1}
          onClick={(e) => {
            e.stopPropagation();
            chevron.onToggle();
          }}
          aria-label={chevron.ariaLabel}
          style={{
            background: "none",
            border: "none",
            padding: 0,
            margin: 0,
            paddingLeft: rem(10 + indent * 16),
            paddingRight: 0,
            display: "inline-flex",
            alignItems: "center",
            color: "inherit",
            cursor: "pointer",
          }}
        >
          {chevron.isCollapsed ? <ChevronRight {...iconProps} /> : <ChevronDown {...iconProps} />}
        </button>
      )}
      <button
        type="button"
        onClick={onClick}
        style={{
          flex: 1,
          minWidth: 0,
          display: "flex",
          alignItems: "center",
          gap: rem(6),
          paddingTop: rem(2),
          paddingBottom: rem(2),
          paddingLeft: chevron ? rem(6) : rem(10 + indent * 16),
          paddingRight: rem(10),
          background: "none",
          border: "none",
          color: "inherit",
          fontSize: rem(15),
          fontFamily: "inherit",
          cursor: "pointer",
          textAlign: "left",
          lineHeight: rem(24),
        }}
      >
        {leading}
        <span
          style={{
            flex: 1,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {label}
        </span>
        {badge != null && <UnreadBadge count={badge} />}
      </button>
    </div>
  );
};

const UnreadBadge: React.FC<{ count: number; muted?: boolean }> = ({ count, muted }) => (
  <span
    style={{
      fontSize: rem(11),
      lineHeight: 1,
      padding: `${rem(2)} ${rem(6)}`,
      borderRadius: "0.25rem",
      background: muted ? "var(--c-text-muted)" : "var(--c-accent)",
      color: "var(--c-bg)",
      flexShrink: 0,
    }}
  >
    {count > 99 ? "99+" : count}
  </span>
);
