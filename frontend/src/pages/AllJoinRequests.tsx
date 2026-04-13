import React, { useMemo } from "react";
import { useAllPendingJoinRequests, useApproveJoinRequest, useRejectJoinRequest } from "../hooks/queries";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";

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

  if (isLoading) {
    return (
      <div
        data-testid="all-join-requests-page"
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: "var(--c-bg)" }}
      >
        <div className="px-6 py-4">
          <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
            Loading…
          </p>
        </div>
      </div>
    );
  }

  if (groupIds.length === 0) {
    return (
      <div
        data-testid="all-join-requests-page"
        className="flex-1 flex flex-col overflow-auto"
        style={{ background: "var(--c-bg)" }}
      >
        <div className="flex-1 flex items-center justify-center">
          <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
            No pending join requests.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div
      data-testid="all-join-requests-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      {groupIds.map((groupId) => {
        const requests = requestsByGroup[groupId];
        const groupName = groupNameById[groupId] ?? groupId;

        return (
          <div key={groupId} className="flex flex-col">
            <div className="px-6 py-4">
              <p className="text-xs font-mono" style={{ color: "var(--c-text-dim)" }}>
                Pending requests to join <span style={{ color: "var(--c-accent)" }}>{groupName}</span>
              </p>
            </div>

            <NavigableList
              items={requests}
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
                <Button
                  data-testid={`approve-request-${req.id}`}
                  onClick={() => handleApprove(req.id, req.group_id)}
                  disabled={approveMutation.isPending || rejectMutation.isPending}
                  variant="primary"
                >
                  approve
                </Button>,
                <Button
                  data-testid={`reject-request-${req.id}`}
                  onClick={() => handleReject(req.id, req.group_id)}
                  disabled={approveMutation.isPending || rejectMutation.isPending}
                  variant="secondary"
                >
                  reject
                </Button>,
              ]}
            />
          </div>
        );
      })}
    </div>
  );
};
