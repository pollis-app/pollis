import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useAppStore } from "../stores/appStore";
import { useLeaveGroup, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const LeaveGroupPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/leave" });
  const { setSelectedGroupId, setSelectedChannelId } = useAppStore();
  const leaveGroupMutation = useLeaveGroup();

  const { data: groupsWithChannels, isLoading } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);

  if (isLoading || !group) {
    return null;
  }

  return (
    <PageShell title="Leave Group">
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        <p className="text-xs font-mono text-center" style={{ color: "var(--c-text-dim)" }}>
          Are you sure you want to leave <strong>{group.name}</strong>?
          <br />
          You will need a new invite to rejoin.
        </p>
        {leaveGroupMutation.isError && (
          <p className="text-xs font-mono" style={{ color: "#ff6b6b" }}>
            {leaveGroupMutation.error instanceof Error ? leaveGroupMutation.error.message : "Failed to leave group"}
          </p>
        )}
        <div className="flex gap-3">
          <Button
            data-testid="leave-group-confirm"
            variant="danger"
            onClick={async () => {
              try {
                await leaveGroupMutation.mutateAsync(group.id);
                setSelectedGroupId(null);
                setSelectedChannelId(null);
                navigate({ to: "/" });
              } catch {
                // error shown via isError above
              }
            }}
            disabled={leaveGroupMutation.isPending}
            isLoading={leaveGroupMutation.isPending}
            loadingText="Leaving…"
          >
            Yes, Leave
          </Button>
          <Button
            data-testid="leave-group-cancel"
            variant="secondary"
            onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          >
            Cancel
          </Button>
        </div>
      </div>
    </PageShell>
  );
};
