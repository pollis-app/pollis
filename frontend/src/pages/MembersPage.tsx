import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Members } from "./Members";
import { PageShell } from "../components/Layout/PageShell";

export const MembersPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { groupId } = useParams({ from: "/groups/$groupId/members" });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (!group) {
    return null;
  }

  return (
    <PageShell title={t("members.pageTitle")} scrollable>
      <div className="pt-8 flex-1 flex flex-col overflow-hidden">
        <Members groupId={groupId} isAdmin={group.current_user_role === "admin"} />
      </div>
    </PageShell>
  );
};
