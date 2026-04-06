import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useKickMember, useGroupMembers } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const KickMemberPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, userId } = useParams({ from: "/groups/$groupId/members/$userId/kick" });
  const kickMutation = useKickMember();
  const { data: members = [] } = useGroupMembers(groupId);
  const member = members.find((m) => m.user_id === userId);

  return (
    <PageShell
      title="Remove Member"
      onBack={() => navigate({ to: "/groups/$groupId/members", params: { groupId } })}
    >
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        <p className="text-xs font-mono text-center" style={{ color: "var(--c-text-dim)" }}>
          Remove <strong>{member?.username ?? userId}</strong> from this group?
          <br />
          They will need a new invite to rejoin.
        </p>
        {kickMutation.isError && (
          <p className="text-xs font-mono" style={{ color: "#ff6b6b" }}>
            {kickMutation.error instanceof Error
              ? kickMutation.error.message
              : "Failed to remove member"}
          </p>
        )}
        <div className="flex gap-3">
          <Button
            data-testid="kick-member-confirm"
            variant="danger"
            onClick={async () => {
              try {
                await kickMutation.mutateAsync({ groupId, userId });
                navigate({ to: "/groups/$groupId/members", params: { groupId } });
              } catch {
                // error shown via isError above
              }
            }}
            disabled={kickMutation.isPending}
            isLoading={kickMutation.isPending}
            loadingText="Removing…"
          >
            Yes, Remove
          </Button>
          <Button
            data-testid="kick-member-cancel"
            variant="secondary"
            onClick={() => navigate({ to: "/groups/$groupId/members", params: { groupId } })}
          >
            Cancel
          </Button>
        </div>
      </div>
    </PageShell>
  );
};
