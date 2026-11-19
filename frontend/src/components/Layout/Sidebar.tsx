import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useRouter, useRouterState } from "@tanstack/react-router";
import {
  FolderPlus,
  Plus,
  Volume2,
  Settings as SettingsIcon,
  Users,
  MessageCircle,
  Palette,
  Globe,
  User as UserIcon,
  ShieldCheck,
  ScrollText,
  History,
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
import type { ActivateOn } from "../../hooks/usePressActivation";
import { PressButton } from "../ui/PressButton";
import { PanelCloseRow } from "../ui/PanelCloseRow";
import { IconButton } from "../ui/IconButton";
import { usePersistentIdSet } from "../../hooks/usePersistentIdSet";
import { SidebarRow as Row, SidebarUnreadBadge as UnreadBadge, sidebarIconProps as iconProps } from "./SidebarRow";
import { SidebarGroupChannels } from "./SidebarGroupChannels";

const COLLAPSED_GROUPS_KEY = "pollis.sidebar.collapsedGroups";
// Per device, like collapsed groups; never synced (#2031).
const COLLAPSED_CATEGORIES_KEY = "pollis.sidebar.collapsedCategories";

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

  const [collapsedGroups, toggleGroup] = usePersistentIdSet(COLLAPSED_GROUPS_KEY);
  const [collapsedCategories, toggleCategory] = usePersistentIdSet(COLLAPSED_CATEGORIES_KEY);
  // The admin "+" on a group row (#2031): its in-place menu, and the inline new-category field.
  const [addMenuGroupId, setAddMenuGroupId] = useState<string | null>(null);
  const [newCategoryGroupId, setNewCategoryGroupId] = useState<string | null>(null);
  const voiceState = appStore.voiceState;
  const connectedVoiceId = voiceState.kind === "idle" ? null : voiceState.channelId;

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
    { id: "language", label: t("sidebar.language"), icon: <Globe {...iconProps} />, to: "/preferences/language" as const, isActive: pathname === "/preferences/language" },
    { id: "user", label: t("sidebar.userSettings"), icon: <UserIcon {...iconProps} />, to: "/user" as const, isActive: pathname === "/user" },
    { id: "voice-settings", label: t("sidebar.voiceAndVideo"), icon: <Volume2 {...iconProps} />, to: "/voice-settings" as const, isActive: pathname === "/voice-settings" },
    { id: "security", label: t("sidebar.security"), icon: <ShieldCheck {...iconProps} />, to: "/security" as const, isActive: pathname === "/security" || (pathname.startsWith("/security/") && pathname !== "/security/events" && pathname !== "/security/copy-history") },
    { id: "security-events", label: t("sidebar.securityEvents"), icon: <ScrollText {...iconProps} />, to: "/security/events" as const, isActive: pathname === "/security/events" },
    { id: "copy-history", label: t("sidebar.copyHistory"), icon: <History {...iconProps} />, to: "/security/copy-history" as const, isActive: pathname === "/security/copy-history" },
    { id: "shortcuts", label: t("sidebar.keyBindings"), icon: <Keyboard {...iconProps} />, to: "/shortcuts" as const, isActive: pathname === "/shortcuts" },
    { id: "update", label: t("sidebar.softwareUpdate"), icon: <Download {...iconProps} />, to: "/update" as const, isActive: pathname === "/update" },
  ];
  // Diagnostics and the open source licences have no sidebar row but are settings pages.
  const isOnAnySettings =
    isOnSettingsHub ||
    pathname === "/diagnostics" ||
    pathname === "/open-source" ||
    pathname.startsWith("/open-source/") ||
    settingsItems.some((s) => s.isActive);

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
            const isAdmin = group.current_user_role === "admin";
            const addMenuOpen = addMenuGroupId === group.id;
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
                  testId={`group-${group.id}`}
                  actionsPinned={addMenuOpen}
                  actions={
                    isAdmin ? (
                      <IconButton
                        aria-label={t("channels:categories.addToGroup", { name: group.name })}
                        aria-expanded={addMenuOpen}
                        data-testid={`sidebar-group-add-${group.id}`}
                        onClick={() => setAddMenuGroupId(addMenuOpen ? null : group.id)}
                      >
                        <Plus {...iconProps} />
                      </IconButton>
                    ) : undefined
                  }
                />
                {addMenuOpen && (
                  <div data-testid={`sidebar-group-add-menu-${group.id}`}>
                    <Row
                      indent={2}
                      isActive={false}
                      leading={<Plus {...iconProps} />}
                      label={t("channels:categories.newChannel")}
                      testId={`group-${group.id}-new-channel`}
                      onClick={() => {
                        setAddMenuGroupId(null);
                        appStore.setSelectedGroupId(group.id);
                        appStore.setNewChannelCategoryId(null);
                        router.navigate({ to: "/groups/$groupId/channels/new", params: { groupId: group.id } });
                      }}
                    />
                    <Row
                      indent={2}
                      isActive={false}
                      leading={<FolderPlus {...iconProps} />}
                      label={t("channels:categories.newCategory")}
                      testId={`group-${group.id}-new-category`}
                      onClick={() => {
                        setAddMenuGroupId(null);
                        if (isCollapsed) {
                          toggleGroup(group.id);
                        }
                        setNewCategoryGroupId(group.id);
                      }}
                    />
                  </div>
                )}
                {!isCollapsed && (
                  <SidebarGroupChannels
                    group={group}
                    isAdmin={isAdmin}
                    collapsedCategories={collapsedCategories}
                    toggleCategory={toggleCategory}
                    activeChannelId={activeChannelId}
                    activeVoiceId={activeVoiceId}
                    connectedVoiceId={connectedVoiceId}
                    unreadCounts={unreadCounts}
                    voiceCounts={voiceCounts}
                    mutedChannels={mutedChannels}
                    creatingCategory={newCategoryGroupId === group.id}
                    onCreatingCategoryDone={() => setNewCategoryGroupId(null)}
                  />
                )}
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
                  skin === "modern" ? (
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
                testId={`dm-${c.id}`}
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

      {skin === "modern" ? (
        <SidebarProfilePanel />
      ) : (
        <PanelCloseRow
          data-testid="sidebar-close"
          onClick={onToggle}
          label={t("common:actions.close")}
          shortcut={toggleSidebarLabel}
          ariaLabel={t("sidebar.close", { shortcut: toggleSidebarLabel })}
        />
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
    <PressButton
      activateOn={activateOn ?? "click"}
      onClick={onClick}
      className={cls}
      data-testid={`sidebar-row-${testId}`}
    >
      {icon}
      <span className="flex-1 leading-[100%] text-[0.8rem]">{label}</span>
      {badge != null && <UnreadBadge count={badge} muted />}
    </PressButton>
  );
};
