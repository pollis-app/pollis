import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useLeaveGroup, useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";

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
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <button
          onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-text)" }}>Leave Group</span>
      </div>
      <div className="flex-1 flex flex-col items-center justify-center gap-4 px-6">
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
    </div>
  );
};
