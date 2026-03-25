import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { LastMessagePreview } from "../components/Message/LastMessagePreview";

export const GroupPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId" });
  const { setSelectedGroupId, setSelectedChannelId, setActiveVoiceChannelId, markRead, unreadCounts } = useAppStore();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

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

  const items: TerminalMenuItem[] = [
    ...channels.map((ch) => ({
      id: ch.id,
      // Voice channels get a [v] prefix; text channels get #
      label: ch.channel_type === "voice" ? `[v] ${ch.name}` : `# ${ch.name}`,
      description: ch.channel_type === "voice"
        ? (ch.description || undefined)
        : <LastMessagePreview channelId={ch.id} />,
      action: () => {
        if (ch.channel_type === "voice") {
          setActiveVoiceChannelId(ch.id);
          navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId: ch.id } });
        } else {
          setSelectedChannelId(ch.id);
          markRead(ch.id);
          navigate({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId: ch.id } });
        }
      },
      badge: ch.channel_type === "voice" ? 0 : (unreadCounts[ch.id] ?? 0),
      testId: `channel-option-${ch.id}`,
    })),
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
