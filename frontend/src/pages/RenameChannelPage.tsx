import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { RenameEntity } from "./RenameEntity";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const RenameChannelPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/channels/$channelId/rename" });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const isVoice = channel?.channel_type === "voice";

  return (
    <PageShell title={t("renameChannel.pageTitle")}>
      <RenameEntity
        kind="channel"
        groupId={groupId}
        channelId={channelId}
        onSuccess={() => {
          if (isVoice) {
            finish({ to: "/groups/$groupId/voice/$channelId", params: { groupId, channelId } });
          } else {
            finish({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId } });
          }
        }}
      />
    </PageShell>
  );
};
