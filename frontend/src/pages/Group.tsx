import React, { useMemo } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Hash, Plus, Volume2, Users, UserPlus, Inbox, LogOut, Pencil, Smile } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useUserGroupsWithChannels, useGroupJoinRequests } from "../hooks/queries/useGroups";
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

  const voiceChannelIds = useMemo(
    () => (group?.channels ?? []).filter((ch) => ch.channel_type === "voice").map((ch) => ch.id),
    [group]
  );
  const { data: voiceCounts = {} } = useVoiceRoomCounts(voiceChannelIds);
  const { data: joinRequests = [] } = useGroupJoinRequests(isAdmin ? groupId : null);

  // One batched preview fetch for the whole channel list (#874) instead of one
  // IPC call per row. Computed above the early returns because hooks must be.
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
      // Selecting (hover or keyboard) a voice row signals intent to maybe
      // join — pre-warm DNS/TLS to LiveKit so the actual Join is fast.
      onSelect: () => warmVoiceChannel(ch.id),
      badge: count > 0 ? count : 0,
      testId: `channel-option-${ch.id}`,
    };
  });

  // Insert a separator before voice channels only when both sections are non-empty
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
    },
    // Any MEMBER can add a custom emoji (#848) — the cost is bounded per
    // person rather than gatekept — so this sits outside the admin block.
    {
      id: "group-emoji",
      label: t("group.customEmoji"),
      icon: <Smile size={14} />,
      action: () => navigate({ to: "/groups/$groupId/emoji", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-group-emoji",
    },
    ...(isAdmin ? [
      {
        id: "rename-group",
        label: t("group.rename"),
        icon: <Pencil size={14} />,
        action: () => navigate({ to: "/groups/$groupId/rename", params: { groupId } }),
        type: "system" as const,
        testId: "menu-item-rename-group",
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
      },
      {
        id: "invite-member",
        label: t("group.inviteMember"),
        icon: <UserPlus size={14} />,
        action: () => navigate({ to: "/groups/$groupId/invite", params: { groupId } }),
        type: "system" as const,
        testId: "menu-item-invite-member",
      },
      {
        id: "join-requests",
        label: t("group.joinRequests"),
        icon: <Inbox size={14} />,
        action: () => navigate({ to: "/groups/$groupId/join-requests", params: { groupId } }),
        badge: joinRequests.length > 0 ? joinRequests.length : undefined,
        type: "system" as const,
        testId: "menu-item-join-requests",
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
