import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { InviteMember } from "./InviteMember";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const InviteMemberPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { groupId } = useParams({ from: "/groups/$groupId/invite" });

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell title={t("inviteMember.pageTitle", { group: group.name })}>
      <InviteMember groupId={group.id} groupName={group.name} />
    </PageShell>
  );
};
