import React, { useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Hash, Plus, Volume2, Users, UserPlus, Inbox, LogOut } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels, useGroupJoinRequests } from "../hooks/queries/useGroups";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";
import { useVoiceRoomCounts } from "../hooks/queries/useVoiceParticipants";

export const GroupPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId" });
  const { setSelectedGroupId, setSelectedChannelId, markRead, unreadCounts } = useAppStore();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const isAdmin = group?.current_user_role === 'admin';

  const voiceChannelIds = useMemo(
    () => (group?.channels ?? []).filter((ch) => ch.channel_type === "voice").map((ch) => ch.id),
    [group]
  );
  const { data: voiceCounts = {} } = useVoiceRoomCounts(voiceChannelIds);
  const { data: joinRequests = [] } = useGroupJoinRequests(isAdmin ? groupId : null);

  if (isLoading) {
    return (
      <TerminalMenu
        items={[{ id: "__loading__", label: "Loading…", disabled: true }]}
      />
    );
  }

  if (!group) {
    return (
      <TerminalMenu
        items={[
          { id: "__not-found__", label: "Group not found", disabled: true },
          {
            id: "__back__",
            label: "Go back",
            icon: <ArrowLeft size={14} />,
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
    description: <LastMessagePreview channelId={ch.id} />,
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
      description: count > 0 ? `${count} in call` : (ch.description || "Join voice chat"),
      action: () => {
        navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId: ch.id } });
      },
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
      label: "Members",
      icon: <Users size={14} />,
      action: () => navigate({ to: "/groups/$groupId/members", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-members",
    },
    ...(isAdmin ? [
      {
        id: "create-channel",
        label: "New Channel",
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
        label: "Invite Member",
        icon: <UserPlus size={14} />,
        action: () => navigate({ to: "/groups/$groupId/invite", params: { groupId } }),
        type: "system" as const,
        testId: "menu-item-invite-member",
      },
      {
        id: "join-requests",
        label: "Join Requests",
        icon: <Inbox size={14} />,
        action: () => navigate({ to: "/groups/$groupId/join-requests", params: { groupId } }),
        badge: joinRequests.length > 0 ? joinRequests.length : undefined,
        type: "system" as const,
        testId: "menu-item-join-requests",
      },
    ] : []),
    {
      id: "leave-group",
      label: "Leave Group",
      icon: <LogOut size={14} />,
      action: () => navigate({ to: "/groups/$groupId/leave", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-leave-group",
    },
    {
      id: "__back__",
      label: "Go back",
      icon: <ArrowLeft size={14} />,
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
};
