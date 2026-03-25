import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const VoiceChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/voice/$channelId" });
  const { activeVoiceChannelId } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const channelName = channel?.name ?? "voice";

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <button
          onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-text)" }}>[v] {channelName}</span>
      </div>
      <div className="flex-1 overflow-hidden">
        <VoiceChannelView
          channelId={activeVoiceChannelId}
          channelName={channelName}
        />
      </div>
    </div>
  );
};
