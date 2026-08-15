import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { JoinRequests } from "./JoinRequests";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const JoinRequestsPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { groupId } = useParams({ from: "/groups/$groupId/join-requests" });

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell title={t("joinRequests.pageTitle", { group: group.name })}>
      <JoinRequests groupId={group.id} groupName={group.name} />
    </PageShell>
  );
};
