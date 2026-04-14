import React from "react";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { InviteMember } from "./InviteMember";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const InviteMemberPage: React.FC = () => {
  const { groupId } = useParams({ from: "/groups/$groupId/invite" });

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell title={`Invite Member :: ${group.name}`}>
      <InviteMember groupId={group.id} groupName={group.name} />
    </PageShell>
  );
};
