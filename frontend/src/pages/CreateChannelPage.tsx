import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { CreateChannel } from "./CreateChannel";

export const CreateChannelPage: React.FC = observer(() => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/channels/new" });
  const { setSelectedChannelId } = appStore;

  return (
    <PageShell title={t("createChannel.pageTitle")} scrollable>
      <CreateChannel
        onSuccess={(channelId, channelType) => {
          if (!channelId || channelType === "voice") {
            navigate({ to: "/groups/$groupId", params: { groupId } });
          } else {
            setSelectedChannelId(channelId);
            navigate({ to: "/groups/$groupId/channels/$channelId", params: { groupId, channelId } });
          }
        }}
      />
    </PageShell>
  );
});
