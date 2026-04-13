import React from "react";
import { usePendingInvites, useAcceptInvite, useDeclineInvite } from "../hooks/queries";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";

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
      style={{ background: "var(--c-bg)" }}
    >
      <NavigableList
        items={invites}
        isLoading={isLoading}
        emptyLabel="No pending invites."
        getKey={(invite) => invite.id}
        rowTestId={(invite) => `invite-${invite.id}`}
        renderRow={(invite) => (
          <div className="flex-1 min-w-0 flex flex-col gap-0.5">
            <span
              className="text-sm font-mono font-medium truncate"
              style={{ color: "var(--c-text)" }}
            >
              {invite.group_name}
            </span>
            <span
              className="text-xs font-mono truncate"
              style={{ color: "var(--c-text-muted)" }}
            >
              Invited by {invite.inviter_username ?? invite.inviter_id}
            </span>
          </div>
        )}
        controls={(invite) => [
          <Button
            data-testid={`accept-invite-${invite.id}`}
            onClick={() => handleAccept(invite.id)}
            disabled={acceptMutation.isPending || declineMutation.isPending}
            variant="primary"
          >
            accept
          </Button>,
          <Button
            data-testid={`decline-invite-${invite.id}`}
            onClick={() => handleDecline(invite.id)}
            disabled={acceptMutation.isPending || declineMutation.isPending}
            variant="secondary"
          >
            decline
          </Button>,
        ]}
      />
    </div>
  );
};
