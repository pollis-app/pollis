import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
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
  ShieldAlert,
  Keyboard,
  Download,
  Bookmark,
  Search,
} from "lucide-react";
import { useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
import { useDMConversations } from "../../hooks/queries/useMessages";
import { useVoiceRoomCounts } from "../../hooks/queries/useVoiceParticipants";
import { usePeerVerificationMap } from "../../hooks/queries/useUserProfile";
import { observer } from "mobx-react-lite";
import { appStore } from "../../stores/appStore";
import { PresenceDot } from "../ui/PresenceDot";
import { useShortcutLabel } from "../../keyboard";
import { useSkin } from "../../hooks/queries/usePreferences";
import { probeRender } from "../../utils/renderProbe";
import { PresenceAvatar } from "../ui/PresenceAvatar";
import { SidebarProfilePanel } from "./SidebarProfilePanel";
import { ResizeHandle } from "./ResizeHandle";
import { usePanelWidth } from "./usePanelWidth";

const COLLAPSED_GROUPS_KEY = "pollis.sidebar.collapsedGroups";

// Sidebar chrome is sized in rem (via Tailwind's rem-based scale + a few
// rem arbitrary values) so it tracks the user's font-size preference
// (`--font-size-base` on :root). px would freeze while the app scales.
// Shared lucide sizing: `size` seeds the SVG attribute; the rem `size-[…]`
// class scales it with the font preference (CSS wins over the attribute).
const iconProps = {
  size: 14,
  className: "size-[0.933rem] shrink-0",
} as const;

// Per-depth LEADING padding (10 + indent*16 px @ 15px base ⇒ rem, scalable).
// Logical (`ps-`), not `pl-`: the indent scheme is the reading-order nesting
// of the tree, so under `dir=rtl` it has to grow from the right edge.
// indent is only ever 1 (group / dm / settings) or 2 (channel).
const indentPadClass = (indent: number): string =>
  indent >= 2 ? "ps-[2.8rem]" : "ps-[1.733rem]";

interface SidebarProps {
  isOpen: boolean;
  onToggle: () => void;
}

export const Sidebar: React.FC<SidebarProps> = observer(({ isOpen, onToggle }) => {
  probeRender("Sidebar");
  const { t } = useTranslation("nav");
  const router = useRouter();
  const pathname = useRouterState({ select: (s) => s.location.pathname });
  const toggleSidebarLabel = useShortcutLabel("app.toggleSidebar");
  const skin = useSkin();
  // Dragging the inner edge past the minimum collapses through the SAME
  // toggle `mod+b` uses, so there is one collapsed state and not a second
  // drag-only one — and reopening restores the width that was dragged from.
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "sidebar",
    onToggle,
  );

  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: dmConversations = [] } = useDMConversations();
  // peerUserId → { verified, key_changed }. Used to glance-render the
  // shield-check (verified) / shield-alert (changed) icons next to each
  // DM row without an N+1 round-trip.
  const verificationByPeer = usePeerVerificationMap();
  const unreadCounts = appStore.unreadCounts;

  // Stable list of voice channel ids across all groups; powers the live
  // "users connected" badge on voice channel rows. Realtime voice events
  // invalidate the `voice-room-counts` query so counts refresh automatically.
  const voiceChannelIds = useMemo(
    () =>
      groupsWithChannels.flatMap((g) =>
        g.channels.filter((ch) => ch.channel_type === "voice").map((ch) => ch.id)
      ),
    [groupsWithChannels]
  );
  const { data: voiceCounts = {} } = useVoiceRoomCounts(voiceChannelIds);

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
    () => appStore.unreadFor(dmConversations),
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
    { id: "search", label: t("sidebar.searchMessages"), icon: <Search {...iconProps} />, to: "/search" as const, isActive: pathname === "/search" },
    { id: "saved", label: t("sidebar.saved"), icon: <Bookmark {...iconProps} />, to: "/saved" as const, isActive: pathname === "/saved" },
    { id: "preferences", label: t("sidebar.preferences"), icon: <Palette {...iconProps} />, to: "/preferences" as const, isActive: pathname === "/preferences" },
    { id: "user", label: t("sidebar.userSettings"), icon: <UserIcon {...iconProps} />, to: "/user" as const, isActive: pathname === "/user" },
    { id: "voice-settings", label: t("sidebar.voiceAndVideo"), icon: <Volume2 {...iconProps} />, to: "/voice-settings" as const, isActive: pathname === "/voice-settings" },
    { id: "security", label: t("sidebar.security"), icon: <ShieldCheck {...iconProps} />, to: "/security" as const, isActive: pathname === "/security" || pathname.startsWith("/security/") },
    { id: "shortcuts", label: t("sidebar.keyBindings"), icon: <Keyboard {...iconProps} />, to: "/shortcuts" as const, isActive: pathname === "/shortcuts" },
    { id: "update", label: t("sidebar.softwareUpdate"), icon: <Download {...iconProps} />, to: "/update" as const, isActive: pathname === "/update" },
  ];
  const isOnAnySettings = isOnSettingsHub || settingsItems.some((s) => s.isActive);

  return (
    <aside
      ref={ref}
      data-testid="sidebar"
      // `relative` anchors the resize handle to this edge; the width is either
      // the `--side-w` token (no answer on this device) or a dragged pixel
      // value, never both.
      className={`relative flex ${widthClass} shrink-0 flex-col border-e border-line bg-surface font-mono`}
      style={widthStyle}
    >
      <div className="flex-1 overflow-y-auto overflow-x-hidden">
        <SectionHeader
          testId="groups"
          label={t("sidebar.groups")}
          icon={<Users {...iconProps} />}
          isActive={isOnGroups}
          onClick={() => router.navigate({ to: "/groups" })}
          borderedBottom
        />

        <ul>
          {groupsWithChannels.map((group) => {
            const isCollapsed = collapsedGroups.has(group.id);
            const groupUnread = appStore.unreadFor(group.channels);
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
                    ariaLabel: isCollapsed
                      ? t("sidebar.expandGroup", { name: group.name })
                      : t("sidebar.collapseGroup", { name: group.name }),
                  }}
                  label={group.name}
                  badge={isCollapsed && groupUnread > 0 ? groupUnread : null}
                />
                {!isCollapsed &&
                  group.channels.map((ch) => {
                    const isVoice = ch.channel_type === "voice";
                    const unread = unreadCounts[ch.id] ?? 0;
                    const voiceCount = isVoice ? voiceCounts[ch.id] ?? 0 : 0;
                    const isActive = isVoice ? activeVoiceId === ch.id : activeChannelId === ch.id;
                    const badge = isVoice
                      ? voiceCount > 0
                        ? voiceCount
                        : null
                      : unread > 0
                        ? unread
                        : null;
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
                        badge={badge}
                      />
                    );
                  })}
              </li>
            );
          })}
        </ul>

        <SectionHeader
          testId="dms"
          label={t("sidebar.dms")}
          icon={<MessageCircle {...iconProps} />}
          isActive={isOnDms}
          onClick={() => router.navigate({ to: "/dms" })}
          badge={totalDmUnread > 0 ? totalDmUnread : null}
          bordered
        />

        <ul>
          {dmConversations.map((c) => {
            const unread = unreadCounts[c.id] ?? 0;
            const verification = c.user2_id ? verificationByPeer.get(c.user2_id) : undefined;
            // Verified badge wins; a `key_changed` mismatch overrides
            // verified (the contact_verification row's `verified` is
            // cleared by check_and_pin on mismatch, so this is mostly a
            // belt-and-braces guard against a stale local cache).
            const trailing = verification?.key_changed ? (
              <span
                data-testid={`dm-verification-changed-${c.id}`}
                title={t("sidebar.keyChanged")}
                className="inline-flex shrink-0 text-[#f0b429]"
              >
                <ShieldAlert {...iconProps} />
              </span>
            ) : verification?.verified ? (
              <span
                data-testid={`dm-verification-verified-${c.id}`}
                title={t("sidebar.verified")}
                className="inline-flex shrink-0 text-accent"
              >
                <ShieldCheck {...iconProps} />
              </span>
            ) : null;
            return (
              <Row
                key={c.id}
                indent={1}
                isActive={activeDmId === c.id}
                onClick={() => router.navigate({ to: "/dms/$conversationId", params: { conversationId: c.id } })}
                leading={
                  // Refined shows the peer's avatar (with the presence dot
                  // anchored to it); terminal keeps the bare dot so rows stay
                  // one text-line tall.
                  skin === "refined" ? (
                    <PresenceAvatar
                      userId={c.user2_id ?? null}
                      avatarKey={c.user2_avatar_url}
                      size={20}
                      alt={t("sidebar.dmAvatarAlt", { name: c.user2_identifier })}
                      testId={`sidebar-dm-avatar-${c.id}`}
                    />
                  ) : (
                    <PresenceDot
                      userId={c.user2_id ?? null}
                      testId={
                        c.user2_id ? `sidebar-presence-${c.user2_id}` : undefined
                      }
                    />
                  )
                }
                label={c.user2_identifier}
                badge={unread > 0 ? unread : null}
                trailing={trailing}
              />
            );
          })}
        </ul>

        <SectionHeader
          testId="account"
          label={t("sidebar.account")}
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
            testId={s.id}
          />
        ))}
      </div>

      {/* Refined anchors the Discord-style identity + voice panel here; the
          sidebar still collapses via the keyboard shortcut. Terminal keeps the
          explicit Close affordance. */}
      {skin === "refined" ? (
        <SidebarProfilePanel />
      ) : (
        <button
          type="button"
          data-testid="sidebar-close"
          onClick={onToggle}
          aria-label={t("sidebar.close", { shortcut: toggleSidebarLabel })}
          title={t("sidebar.close", { shortcut: toggleSidebarLabel })}
          className="flex shrink-0 items-center gap-2 px-2.5 min-h-composer-flush border-t border-line text-xs text-start cursor-pointer transition-colors text-muted hover:text-fg"
        >
          <span className="flex-1">{t("common:actions.close")}</span>
          <kbd
            aria-hidden="true"
            className="font-mono font-machine bg-bg px-1.5 py-px rounded-[3px] border border-line text-2xs leading-[1.2]"
          >
            {toggleSidebarLabel}
          </kbd>
        </button>
      )}

      <ResizeHandle edge="end" label={t("sidebar.resize")} {...handleProps} />
    </aside>
  );
});

interface SectionHeaderProps {
  /** Stable id for the `data-testid`. Deliberately separate from `label`,
      which is translated and must never move a test selector. */
  testId: string;
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

const SectionHeader: React.FC<SectionHeaderProps> = ({ testId, label, icon, isActive, onClick, badge, bordered, borderedBottom }) => {
  const cls = [
    "sticky top-0 z-[1] flex w-full h-bar items-center gap-1.5 px-2.5",
    "uppercase tracking-[0.08em] text-start cursor-pointer",
    "transition-colors duration-75 bg-surface hover:bg-hover",
    isActive ? "text-accent" : "text-muted",
    bordered ? "mt-1 border-t border-line" : "",
    bordered || borderedBottom ? "border-b border-line" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      type="button"
      onClick={onClick}
      className={cls}
      data-testid={`sidebar-row-${testId}`}
    >
      {icon}
      <span className="flex-1 leading-[100%] text-[0.8rem]">{label}</span>
      {badge != null && <UnreadBadge count={badge} muted />}
    </button>
  );
};

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
  /** Optional trailing decoration (e.g. shield-check / shield-alert badges) rendered before the unread badge. */
  trailing?: React.ReactNode;
  /** Stable id for the navigating button's `data-testid`, same
      `sidebar-row-*` scheme as `SectionHeader` (#932). Deliberately separate
      from `label`, which is translated: since #855 a locale change would
      otherwise break every navigation test that located a row by its text,
      and the failure reads as a routing bug rather than a selector one.
      Omitted for group / channel / DM rows, whose labels are user-generated
      and therefore already locale-independent. */
  testId?: string;
}

const Row: React.FC<RowProps> = ({ indent, isActive, onClick, leading, chevron, label, badge, trailing, testId }) => {
  return (
    <div
      data-active={isActive ? "true" : "false"}
      className={`sidebar-row flex w-full items-stretch border-s-2 transition-colors ${
        isActive
          ? "bg-hover border-accent text-accent"
          : "bg-transparent border-transparent text-fg hover:bg-hover"
      }`}
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
          className={`inline-flex items-center pe-0 cursor-pointer text-inherit ${indentPadClass(indent)}`}
        >
          {chevron.isCollapsed ? (
            <ChevronRight {...iconProps} className={`${iconProps.className} rtl-mirror`} />
          ) : (
            <ChevronDown {...iconProps} />
          )}
        </button>
      )}
      <button
        type="button"
        onClick={onClick}
        data-testid={testId ? `sidebar-row-${testId}` : undefined}
        className={`flex flex-1 min-w-0 items-center gap-1.5 py-0.5 pe-2.5 text-base text-start cursor-pointer text-inherit ${
          chevron ? "ps-[0.4rem]" : indentPadClass(indent)
        }`}
      >
        {leading}
        {/* Group / channel / DM names are user-generated: isolate them so a
            Latin name in an Arabic sidebar (or the reverse) cannot drag the
            trailing badge to the wrong end of the row. */}
        <bdi className="flex-1 truncate">{label}</bdi>
        {trailing}
        {badge != null && <UnreadBadge count={badge} />}
      </button>
    </div>
  );
};

const UnreadBadge: React.FC<{ count: number; muted?: boolean }> = ({ count, muted }) => (
  <span
    className={`shrink-0 rounded px-1.5 py-0.5 text-2xs leading-none text-bg ${
      muted ? "bg-muted" : "bg-accent"
    }`}
  >
    {count > 99 ? "99+" : count}
  </span>
);
