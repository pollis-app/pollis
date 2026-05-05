import React, { useEffect } from "react";
import { useParams } from "@tanstack/react-router";
import { Trash2 } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { useAppStore } from "../stores/appStore";

export const ChannelPage: React.FC = () => {
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId" });
  const setSelectedChannelId = useAppStore((s) => s.setSelectedChannelId);
  const pendingDeleteChannelId = useAppStore((s) => s.pendingDeleteChannelId);
  const setPendingDeleteChannelId = useAppStore((s) => s.setPendingDeleteChannelId);

  useEffect(() => {
    setSelectedChannelId(channelId);
    return () => { setSelectedChannelId(null); };
  }, [channelId, setSelectedChannelId]);

  useEffect(() => {
    return () => { setPendingDeleteChannelId(null); };
  }, [channelId, setPendingDeleteChannelId]);

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const isAdmin = group?.current_user_role === "admin";

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
        <span className="flex-1">{title}</span>
        {isAdmin && channel && pendingDeleteChannelId !== channelId && (
          <button
            data-testid="delete-channel-trigger"
            onClick={() => setPendingDeleteChannelId(channelId)}
            aria-label="Delete channel"
            className="icon-btn-sm flex-shrink-0"
          >
            <Trash2 size={14} aria-hidden="true" />
          </button>
        )}
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
};
