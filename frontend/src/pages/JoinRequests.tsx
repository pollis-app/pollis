import React from "react";
import { useGroupJoinRequests, useApproveJoinRequest, useRejectJoinRequest } from "../hooks/queries";
import { Card } from "../components/ui/Card";
import { Button } from "../components/ui/Button";

interface JoinRequestsProps {
  groupId: string;
  groupName: string;
}

export const JoinRequests: React.FC<JoinRequestsProps> = ({ groupId, groupName }) => {
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

  return (
    <div
      data-testid="join-requests-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-4">

          <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
            Pending requests to join <span style={{ color: 'var(--c-accent)' }}>{groupName}</span>
          </p>

          {isLoading && (
            <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Loading…</p>
          )}

          {!isLoading && requests.length === 0 && (
            <p data-testid="join-requests-empty" className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
              No pending requests.
            </p>
          )}

          {requests.map((req) => (
            <Card
              key={req.id}
              data-testid={`join-request-${req.id}`}
              className="flex items-center justify-between gap-4"
              padding="sm"
            >
              <span className="text-xs font-mono" style={{ color: 'var(--c-text)' }}>
                {req.requester_username ?? req.requester_id}
              </span>
              <div className="flex gap-4 flex-shrink-0">
                <Button
                  data-testid={`approve-request-${req.id}`}
                  onClick={() => handleApprove(req.id)}
                  disabled={approveMutation.isPending || rejectMutation.isPending}
                  variant="primary"
                >
                  Approve
                </Button>
                <Button
                  data-testid={`reject-request-${req.id}`}
                  onClick={() => handleReject(req.id)}
                  disabled={approveMutation.isPending || rejectMutation.isPending}
                  variant="secondary"
                >
                  Reject
                </Button>
              </div>
            </Card>
          ))}
        </div>
      </div>
    </div>
  );
};
