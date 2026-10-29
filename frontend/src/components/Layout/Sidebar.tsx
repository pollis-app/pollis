import React, { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useRouter, useRouterState } from "@tanstack/react-router";
import {
  ChevronDown,
  ChevronRight,
  BellOff,
  Hash,
  Volume2,
  Settings as SettingsIcon,
  Users,
  MessageCircle,
  Palette,
  User as UserIcon,
  ShieldCheck,
  ScrollText,
  ShieldAlert,
  Keyboard,
  Download,
  Bookmark,
  Vault as VaultIcon,
} from "lucide-react";
import { useMutedChannels, useUserGroupsWithChannels } from "../../hooks/queries/useGroups";
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
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

const COLLAPSED_GROUPS_KEY = "pollis.sidebar.collapsedGroups";

// Sized in rem to track the font preference; the rem `size-[…]` class beats the SVG attribute.
const iconProps = {
  size: 14,
  className: "size-[0.933rem] shrink-0",
} as const;

// Logical `ps-` indent, so RTL nests from the right.
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
  // Collapsing via drag uses the same toggle as mod+b: one collapsed state.
  const { ref, widthClass, widthStyle, handleProps } = usePanelWidth(
    "sidebar",
    onToggle,
  );

  const closePress = usePressActivation(onToggle, "press");

  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const { data: mutedChannels } = useMutedChannels();
  const { data: dmConversations = [] } = useDMConversations();
  // peerUserId → verification, without an N+1 round-trip.
  const verificationByPeer = usePeerVerificationMap();
  const unreadCounts = appStore.unreadCounts;

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
    { id: "saved", label: t("sidebar.saved"), icon: <Bookmark {...iconProps} />, to: "/saved" as const, isActive: pathname === "/saved" },
    { id: "preferences", label: t("sidebar.preferences"), icon: <Palette {...iconProps} />, to: "/preferences" as const, isActive: pathname === "/preferences" },
    { id: "user", label: t("sidebar.userSettings"), icon: <UserIcon {...iconProps} />, to: "/user" as const, isActive: pathname === "/user" },
    { id: "voice-settings", label: t("sidebar.voiceAndVideo"), icon: <Volume2 {...iconProps} />, to: "/voice-settings" as const, isActive: pathname === "/voice-settings" },
    { id: "security", label: t("sidebar.security"), icon: <ShieldCheck {...iconProps} />, to: "/security" as const, isActive: pathname === "/security" || (pathname.startsWith("/security/") && pathname !== "/security/events") },
    { id: "security-events", label: t("sidebar.securityEvents"), icon: <ScrollText {...iconProps} />, to: "/security/events" as const, isActive: pathname === "/security/events" },
    { id: "shortcuts", label: t("sidebar.keyBindings"), icon: <Keyboard {...iconProps} />, to: "/shortcuts" as const, isActive: pathname === "/shortcuts" },
    { id: "update", label: t("sidebar.softwareUpdate"), icon: <Download {...iconProps} />, to: "/update" as const, isActive: pathname === "/update" },
  ];
  // Diagnostics has no sidebar row but is a settings page.
  const isOnAnySettings = isOnSettingsHub || pathname === "/diagnostics" || settingsItems.some((s) => s.isActive);

  return (
    <aside
      ref={ref}
      data-testid="sidebar"
      className={`relative flex ${widthClass} shrink-0 flex-col border-e border-line bg-surface font-mono`}
      style={widthStyle}
    >
      <div className="flex-1 overflow-y-auto overflow-x-hidden">
        <SectionHeader
          activateOn="press"
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
                  activateOn="press"
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
                    // Muted (#1431): dimmed plus a bell-off mark, not colour alone.
                    const isMuted = mutedChannels?.has(ch.id) ?? false;
                    const badge = isVoice
                      ? voiceCount > 0
                        ? voiceCount
                        : null
                      : unread > 0
                        ? unread
                        : null;
                    return (
                      // Voice can auto-join (others see it), so stays on click (#1529).
                      <Row
                        activateOn={isVoice ? "click" : "press"}
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
                        dimmed={isMuted}
                        trailing={
                          isMuted ? (
                            <span
                              data-testid={`sidebar-channel-muted-${ch.id}`}
                              title={t("channels:channel.mutedHint")}
                              aria-label={t("channels:channel.mutedHint")}
                              className="inline-flex shrink-0"
                            >
                              <BellOff {...iconProps} />
                            </span>
                          ) : null
                        }
                      />
                    );
                  })}
              </li>
            );
          })}
        </ul>

        <SectionHeader
          activateOn="press"
          testId="dms"
          label={t("sidebar.dms")}
          icon={<MessageCircle {...iconProps} />}
          isActive={isOnDms}
          onClick={() => router.navigate({ to: "/dms" })}
          badge={totalDmUnread > 0 ? totalDmUnread : null}
          bordered
        />

        <ul>
          {/* The Vault (#107): top of the conversation list, like Note to Self. */}
          <Row
            activateOn="press"
            indent={1}
            isActive={pathname === "/vault"}
            leading={<VaultIcon {...iconProps} />}
            label={t("sidebar.vault")}
            testId="vault"
            onClick={() => router.navigate({ to: "/vault" })}
          />
          {dmConversations.map((c) => {
            const unread = unreadCounts[c.id] ?? 0;
            const verification = c.user2_id ? verificationByPeer.get(c.user2_id) : undefined;
            // key_changed overrides verified.
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
                activateOn="press"
                key={c.id}
                indent={1}
                isActive={activeDmId === c.id}
                onClick={() => router.navigate({ to: "/dms/$conversationId", params: { conversationId: c.id } })}
                leading={
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
          activateOn="press"
          testId="account"
          label={t("sidebar.account")}
          icon={<SettingsIcon {...iconProps} />}
          isActive={isOnAnySettings}
          onClick={() => router.navigate({ to: "/settings" })}
          bordered
        />
        {settingsItems.map((s) => (
          <Row
            activateOn="press"
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

      {skin === "refined" ? (
        <SidebarProfilePanel />
      ) : (
        <button
          type="button"
          data-testid="sidebar-close"
          {...closePress}
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
  /** For `data-testid`; never the translated `label`. */
  testId: string;
  label: string;
  icon: React.ReactNode;
  isActive: boolean;
  onClick: () => void;
  badge?: number | null;
  bordered?: boolean;
  borderedBottom?: boolean;
  activateOn?: ActivateOn;
}

const SectionHeader: React.FC<SectionHeaderProps> = ({ testId, label, icon, isActive, onClick, badge, bordered, borderedBottom, activateOn }) => {
  const press = usePressActivation(onClick, activateOn);
  const cls = [
    "sticky top-0 z-[1] flex w-full h-bar items-center gap-1.5 px-2.5",
    "uppercase tracking-[0.08em] text-start cursor-pointer",
    " bg-surface hover:bg-hover",
    isActive ? "text-accent" : "text-muted",
    bordered ? "mt-1 border-t border-line" : "",
    bordered || borderedBottom ? "border-b border-line" : "",
  ]
    .filter(Boolean)
    .join(" ");
  return (
    <button
      type="button"
      {...press}
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
  chevron?: RowChevron;
  label: string;
  badge?: number | null;
  trailing?: React.ReactNode;
  /** `data-testid` (`sidebar-row-*`, #932), separate from the translated label (#855). */
  testId?: string;
  /** A muted channel (#1431). */
  dimmed?: boolean;
  /** `"press"`: pointer-down (#1529). */
  activateOn?: ActivateOn;
}

const Row: React.FC<RowProps> = ({ indent, isActive, onClick, leading, chevron, label, badge, trailing, testId, dimmed, activateOn }) => {
  const press = usePressActivation(onClick, activateOn);
  const chevronPress = usePressActivation(
    chevron
      ? (e: React.MouseEvent<HTMLButtonElement>) => {
          e.stopPropagation();
          chevron.onToggle();
        }
      : undefined,
    activateOn,
  );
  return (
    <div
      data-active={isActive ? "true" : "false"}
      data-muted={dimmed ? "true" : undefined}
      className={`sidebar-row flex w-full items-stretch border-s-2 transition-colors ${
        isActive
          ? "bg-hover border-accent text-accent"
          : `bg-transparent border-transparent hover:bg-hover ${dimmed ? "text-muted" : "text-fg"}`
      }`}
    >
      {chevron && (
        <button
          type="button"
          tabIndex={-1}
          {...chevronPress}
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
        {...press}
        data-testid={testId ? `sidebar-row-${testId}` : undefined}
        className={`flex flex-1 min-w-0 items-center gap-1.5 py-0.5 pe-2.5 text-base text-start cursor-pointer text-inherit ${
          chevron ? "ps-[0.4rem]" : indentPadClass(indent)
        }`}
      >
        {leading}
        {/* Isolated: user-generated names must not drag the badge in mixed-direction text. */}
        <bdi className="flex-1 truncate">{label}</bdi>
        {trailing}
        {badge != null && <UnreadBadge count={badge} muted={dimmed} />}
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
