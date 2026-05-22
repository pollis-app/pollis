import React from "react";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { useGroupJoinRequests, useApproveJoinRequest, useRejectJoinRequest } from "../hooks/queries";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";

export const JoinRequestsPage: React.FC = () => {
  const { groupId } = useParams({ from: "/groups/$groupId/join-requests" });
  const { data: groupsWithChannels, isLoading: groupsLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const { data: requests = [], isLoading } = useGroupJoinRequests(groupId);
  const approveMutation = useApproveJoinRequest();
  const rejectMutation = useRejectJoinRequest();

  const handleApprove = async (requestId: string) => {
    try {
      await approveMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to approve request:", err);
    }
  };

  const handleReject = async (requestId: string) => {
    try {
      await rejectMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to reject request:", err);
    }
  };

  if (groupsLoading || !group) {
    return null;
  }

  return (
    <PageShell title={`Join Requests :: ${group.name}`}>
      <div
        data-testid="join-requests-page"
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: "var(--c-bg)" }}
      >
        <div className="px-4 py-4">
          <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
            Pending requests to join <span style={{ color: "var(--c-accent)" }}>{group.name}</span>
          </p>
        </div>

        <NavigableList
          items={requests}
          isLoading={isLoading}
          emptyLabel="No pending requests."
          getKey={(req) => req.id}
          rowTestId={(req) => `join-request-${req.id}`}
          renderRow={(req) => (
            <span
              className="flex-1 truncate text-xs font-mono"
              style={{ color: "var(--c-text)" }}
            >
              {req.requester_username ?? req.requester_id}
            </span>
          )}
          controls={(req) => [
            <Button size="sm"
              data-testid={`approve-request-${req.id}`}
              onClick={() => handleApprove(req.id)}
              disabled={approveMutation.isPending || rejectMutation.isPending}
              variant="primary"
            >
              approve
            </Button>,
            <Button size="sm"
              data-testid={`reject-request-${req.id}`}
              onClick={() => handleReject(req.id)}
              disabled={approveMutation.isPending || rejectMutation.isPending}
              variant="secondary"
            >
              reject
            </Button>,
          ]}
        />
      </div>
    </PageShell>
  );
};
