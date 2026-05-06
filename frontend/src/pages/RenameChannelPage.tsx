import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { RenameChannel } from "./RenameChannel";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const RenameChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId/rename" });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const isVoice = channel?.channel_type === "voice";

  return (
    <PageShell title="Rename Channel">
      <RenameChannel
        groupId={groupId}
        channelId={channelId}
        onSuccess={() => {
          if (isVoice) {
            navigate({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId } });
          } else {
            navigate({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId } });
          }
        }}
      />
    </PageShell>
  );
};
