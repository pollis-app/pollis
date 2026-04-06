import React, { useMemo } from "react";
import { useAllPendingJoinRequests, useApproveJoinRequest, useRejectJoinRequest } from "../hooks/queries";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Card } from "../components/ui/Card";
import { Button } from "../components/ui/Button";

export const AllJoinRequests: React.FC = () => {
  const { data: allRequests = [], isLoading } = useAllPendingJoinRequests();
  const { data: groupsWithChannels = [] } = useUserGroupsWithChannels();
  const approveMutation = useApproveJoinRequest();
  const rejectMutation = useRejectJoinRequest();

  // Build a map of groupId → group name for display
  const groupNameById = useMemo(() => {
    const map: Record<string, string> = {};
    for (const g of groupsWithChannels) {
      map[g.id] = g.name;
    }
    return map;
  }, [groupsWithChannels]);

  // Group requests by group_id
  const requestsByGroup = useMemo(() => {
    const grouped: Record<string, typeof allRequests> = {};
    for (const req of allRequests) {
      if (!grouped[req.group_id]) {
        grouped[req.group_id] = [];
      }
      grouped[req.group_id].push(req);
    }
    return grouped;
  }, [allRequests]);

  const groupIds = Object.keys(requestsByGroup);

  const handleApprove = async (requestId: string, groupId: string) => {
    try {
      await approveMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to approve request:", err);
    }
  };

  const handleReject = async (requestId: string, groupId: string) => {
    try {
      await rejectMutation.mutateAsync({ requestId, groupId });
    } catch (err) {
      console.error("Failed to reject request:", err);
    }
  };

  return (
    <div
      data-testid="all-join-requests-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-6">

          {isLoading && (
            <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>Loading…</p>
          )}

          {!isLoading && groupIds.length === 0 && (
            <p data-testid="all-join-requests-empty" className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
              No pending join requests.
            </p>
          )}

          {groupIds.map((groupId) => {
            const requests = requestsByGroup[groupId];
            const groupName = groupNameById[groupId] ?? groupId;

            return (
              <div key={groupId} className="flex flex-col gap-3">
                <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
                  Pending requests to join <span style={{ color: "var(--c-accent)" }}>{groupName}</span>
                </p>

                {requests.map((req) => (
                  <Card
                    key={req.id}
                    data-testid={`join-request-${req.id}`}
                    className="flex items-center justify-between gap-4"
                    padding="sm"
                  >
                    <span className="text-xs font-mono" style={{ color: "var(--c-text)" }}>
                      {req.requester_username ?? req.requester_id}
                    </span>
                    <div className="flex gap-2 flex-shrink-0">
                      <Button
                        data-testid={`approve-request-${req.id}`}
                        onClick={() => handleApprove(req.id, req.group_id)}
                        disabled={approveMutation.isPending || rejectMutation.isPending}
                        variant="primary"
                      >
                        Approve
                      </Button>
                      <Button
                        data-testid={`reject-request-${req.id}`}
                        onClick={() => handleReject(req.id, req.group_id)}
                        disabled={approveMutation.isPending || rejectMutation.isPending}
                        variant="secondary"
                      >
                        Reject
                      </Button>
                    </div>
                  </Card>
                ))}
              </div>
            );
          })}

        </div>
      </div>
    </div>
  );
};
