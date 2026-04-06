import React from "react";
import { usePendingInvites, useAcceptInvite, useDeclineInvite } from "../hooks/queries";
import { Card } from "../components/ui/Card";
import { Button } from "../components/ui/Button";

export const Invites: React.FC = () => {
  const { data: invites = [], isLoading } = usePendingInvites();
  const acceptMutation = useAcceptInvite();
  const declineMutation = useDeclineInvite();

  const handleAccept = async (inviteId: string) => {
    try {
      await acceptMutation.mutateAsync(inviteId);
    } catch (err) {
      console.error("Failed to accept invite:", err);
    }
  };

  const handleDecline = async (inviteId: string) => {
    try {
      await declineMutation.mutateAsync(inviteId);
    } catch (err) {
      console.error("Failed to decline invite:", err);
    }
  };

  return (
    <div
      data-testid="invites-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-4">

          {isLoading && (
            <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Loading…</p>
          )}

          {!isLoading && invites.length === 0 && (
            <p data-testid="invites-empty" className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
              No pending invites.
            </p>
          )}

          {invites.map((invite) => (
            <Card
              key={invite.id}
              data-testid={`invite-${invite.id}`}
              className="flex flex-col gap-3"
              padding="sm"
            >
              <div className="flex flex-col gap-0.5">
                <span className="text-sm font-mono font-medium" style={{ color: 'var(--c-text)' }}>
                  {invite.group_name}
                </span>
                <span className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                  Invited by {invite.inviter_username ?? invite.inviter_id}
                </span>
              </div>
              <div className="flex gap-4">
                <Button
                  data-testid={`accept-invite-${invite.id}`}
                  onClick={() => handleAccept(invite.id)}
                  disabled={acceptMutation.isPending || declineMutation.isPending}
                  variant="primary"
                >
                  Accept
                </Button>
                <Button
                  data-testid={`decline-invite-${invite.id}`}
                  onClick={() => handleDecline(invite.id)}
                  disabled={acceptMutation.isPending || declineMutation.isPending}
                  variant="secondary"
                >
                  Decline
                </Button>
              </div>
            </Card>
          ))}
        </div>
      </div>
    </div>
  );
};
