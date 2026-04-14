import React, { useEffect } from "react";
import { useParams } from "@tanstack/react-router";
import { MainContent } from "../components/Layout/MainContent";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { useAppStore } from "../stores/appStore";

export const ChannelPage: React.FC = () => {
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId" });
  const setSelectedChannelId = useAppStore((s) => s.setSelectedChannelId);

  useEffect(() => {
    setSelectedChannelId(channelId);
    return () => { setSelectedChannelId(null); };
  }, [channelId, setSelectedChannelId]);

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);

  const title = channel ? channel.name : "Channel";

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <span>{title}</span>
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
};
