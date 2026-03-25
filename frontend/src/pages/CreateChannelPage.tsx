import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { useAppStore } from "../stores/appStore";
import { CreateChannel } from "./CreateChannel";

export const CreateChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/channels/new" });
  const { setSelectedChannelId } = useAppStore();

  return (
    <PageShell
      title="New Channel"
      onBack={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
    >
      <CreateChannel
        onSuccess={(channelId) => {
          if (channelId) {
            setSelectedChannelId(channelId);
            navigate({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId } });
          } else {
            navigate({ to: "/groups/$groupId", params: { groupId } });
          }
        }}
      />
    </PageShell>
  );
};
