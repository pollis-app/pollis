import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { JoinRequests } from "./JoinRequests";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

export const JoinRequestsPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/join-requests" });

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell
      title={`Join Requests :: ${group.name}`}
      onBack={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
    >
      <JoinRequests groupId={group.id} groupName={group.name} />
    </PageShell>
  );
};
