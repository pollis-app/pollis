import React, { useMemo } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, Hash, Volume2 } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";
import { useVoiceRoomCounts } from "../hooks/queries/useVoiceParticipants";

export const GroupPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId" });
  const { setSelectedGroupId, setSelectedChannelId, setActiveVoiceChannelId, markRead, unreadCounts } = useAppStore();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  const voiceChannelIds = useMemo(
    () => (group?.channels ?? []).filter((ch) => ch.channel_type === "voice").map((ch) => ch.id),
    [group]
  );
  const { data: voiceCounts = {} } = useVoiceRoomCounts(voiceChannelIds);

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
        setActiveVoiceChannelId(ch.id);
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
      id: "create-channel",
      label: "+ New Channel",
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
      action: () => navigate({ to: "/groups/$groupId/invite", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-invite-member",
    },
    {
      id: "join-requests",
      label: "Join Requests",
      action: () => navigate({ to: "/groups/$groupId/join-requests", params: { groupId } }),
      type: "system" as const,
      testId: "menu-item-join-requests",
    },
    {
      id: "leave-group",
      label: "Leave Group",
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
