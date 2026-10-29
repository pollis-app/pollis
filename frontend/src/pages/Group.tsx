import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, BellOff, Hash, Plus, Volume2, Users, UserPlus, Inbox, LogOut, Pencil, Smile } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useUserGroupsWithChannels, useGroupJoinRequests, useMutedChannels } from "../hooks/queries/useGroups";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";
import { useLastMessages } from "../hooks/queries/useMessages";
import { useVoiceRoomCounts } from "../hooks/queries/useVoiceParticipants";
import { warmVoiceChannel } from "../utils/voiceWarmup";

export const GroupPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId" });
  const { setSelectedGroupId, setSelectedChannelId, markRead, unreadCounts } = appStore;

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const isAdmin = group?.current_user_role === 'admin';
  const { data: mutedChannels } = useMutedChannels();

  const voiceChannelIds = useMemo(
    () => (group?.channels ?? []).filter((ch) => ch.channel_type === "voice").map((ch) => ch.id),
    [group]
  );
  const { data: voiceCounts = {} } = useVoiceRoomCounts(voiceChannelIds);
  const { data: joinRequests = [] } = useGroupJoinRequests(isAdmin ? groupId : null);

  // One batched preview fetch (#874); above the early returns since it's a hook.
  const previewChannelIds = useMemo(
    () => (group?.channels ?? []).filter((ch) => ch.channel_type !== "voice").map((ch) => ch.id),
    [group]
  );
  const { data: lastMessages = {}, isLoading: previewsLoading } =
    useLastMessages(previewChannelIds);

  if (isLoading) {
    return (
      <TerminalMenu
        items={[{ id: "__loading__", label: t("common:states.loading"), disabled: true }]}
      />
    );
  }

  if (!group) {
    return (
      <TerminalMenu
        items={[
          { id: "__not-found__", label: t("group.notFound"), disabled: true },
          {
            id: "__back__",
            activateOn: "press" as const,
            label: t("common:actions.goBack"),
            icon: <ArrowLeft size={14} className="rtl-mirror" />,
            action: () => navigate({ to: "/groups" }),
            type: "system",
          },
        ]}
        onEsc={() => navigate({ to: "/groups" })}
      />
    );
  }

  const channels = group.channels ?? [];
  const textChannels = channels.filter((ch) => ch.channel_type !== "voice");
  const voiceChannels = channels.filter((ch) => ch.channel_type === "voice");

  const textChannelItems: TerminalMenuItem[] = textChannels.map((ch) => ({
    id: ch.id,
    label: ch.name,
    icon: <Hash size={14} />,
    description: <LastMessagePreview message={lastMessages[ch.id]} isLoading={previewsLoading} />,
    action: () => {
      setSelectedChannelId(ch.id);
      markRead(ch.id);
      navigate({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId: ch.id } });
    },
    badge: unreadCounts[ch.id] ?? 0,
    testId: `channel-option-${ch.id}`,
    // Text channels open on press (#1529); voice rows may auto-join, so they stay on click.
    activateOn: "press" as const,
    // Muted (#1431): dimmed plus a bell-off mark, not colour alone.
    ...(mutedChannels?.has(ch.id)
      ? {
        type: "system" as const,
        trailingIndicator: (
          <span title={t("channel.mutedHint")} aria-label={t("channel.mutedHint")} className="text-muted">
            <BellOff size={12} />
          </span>
        ),
      }
      : {}),
  }));

  const voiceChannelItems: TerminalMenuItem[] = voiceChannels.map((ch) => {
    const count = voiceCounts[ch.id] ?? 0;
    return {
      id: ch.id,
      label: ch.name,
      icon: <Volume2 size={14} />,
      description:
        count > 0
          ? t("group.inCall", { count })
          : (ch.description || t("group.joinVoiceChat")),
      action: () => {
        navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId: ch.id } });
      },
      // Pre-warm LiveKit on voice-row intent.
      onSelect: () => warmVoiceChannel(ch.id),
      badge: count > 0 ? count : 0,
      testId: `channel-option-${ch.id}`,
    };
  });

  const voiceSeparator: TerminalMenuItem[] =
    textChannels.length > 0 && voiceChannels.length > 0
      ? [{ id: "__voice-sep__", label: "", type: "separator" as const }]
      : [];

  const items: TerminalMenuItem[] = [
    ...textChannelItems,
    ...voiceSeparator,
    ...voiceChannelItems,
    { id: "__sep__", label: "", type: "separator" as const },
    {
      id: "members",
      label: t("group.members"),
      icon: <Users size={14} />,
      action: () => navigate({ to: "/groups/$groupId/members", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-members",
      activateOn: "press" as const,
    },
    // Any member may add emoji (#848), so outside the admin block.
    {
      id: "group-emoji",
      label: t("group.customEmoji"),
      icon: <Smile size={14} />,
      action: () => navigate({ to: "/groups/$groupId/emoji", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-group-emoji",
      activateOn: "press" as const,
    },
    ...(isAdmin ? [
      {
        id: "rename-group",
        label: t("group.rename"),
        icon: <Pencil size={14} />,
        action: () => navigate({ to: "/groups/$groupId/rename", params: { groupId } }),
        type: "system" as const,
        testId: "menu-item-rename-group",
        activateOn: "press" as const,
      },
      {
        id: "create-channel",
        label: t("group.newChannel"),
        icon: <Plus size={14} />,
        action: () => {
          setSelectedGroupId(group.id);
          navigate({ to: "/groups/$groupId/channels/new", params: { groupId } });
        },
        type: "system" as const,
        testId: "menu-item-create-channel",
        activateOn: "press" as const,
      },
      {
        id: "invite-member",
        label: t("group.inviteMember"),
        icon: <UserPlus size={14} />,
        action: () => navigate({ to: "/groups/$groupId/invite", params: { groupId } }),
        type: "system" as const,
        testId: "menu-item-invite-member",
        activateOn: "press" as const,
      },
      {
        id: "join-requests",
        label: t("group.joinRequests"),
        icon: <Inbox size={14} />,
        action: () => navigate({ to: "/groups/$groupId/join-requests", params: { groupId } }),
        badge: joinRequests.length > 0 ? joinRequests.length : undefined,
        type: "system" as const,
        testId: "menu-item-join-requests",
        activateOn: "press" as const,
      },
    ] : []),
    {
      id: "leave-group",
      label: t("group.leave"),
      icon: <LogOut size={14} />,
      action: () => navigate({ to: "/groups/$groupId/leave", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-leave-group",
    },
    {
      id: "__back__",
      activateOn: "press" as const,
      label: t("common:actions.goBack"),
      icon: <ArrowLeft size={14} className="rtl-mirror" />,
      action: () => navigate({ to: "/groups" }),
      type: "system",
    },
  ];

  return (
    <TerminalMenu
      items={items}
      onEsc={() => navigate({ to: "/groups" })}
    />
  );
});
