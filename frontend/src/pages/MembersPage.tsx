import React from "react";
import { useParams } from "@tanstack/react-router";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Members } from "./Members";
import { PageShell } from "../components/Layout/PageShell";

export const MembersPage: React.FC = () => {
  const { groupId } = useParams({ from: "/groups/$groupId/members" });
  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (!group) {
    return null;
  }

  return (
    <PageShell title="Members" scrollable>
      <div className="pt-8 flex-1 flex flex-col overflow-hidden">
        <Members groupId={groupId} isAdmin={group.current_user_role === "admin"} />
      </div>
    </PageShell>
  );
};
