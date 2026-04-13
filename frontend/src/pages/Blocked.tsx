import React from "react";
import { useBlockedUsers, useUnblockUser } from "../hooks/queries";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";
import { timeAgo } from "../utils/timeAgo";

export const Blocked: React.FC = () => {
  const { data: blocked = [], isLoading } = useBlockedUsers();
  const unblockMutation = useUnblockUser();

  const handleUnblock = async (userId: string) => {
    try {
      await unblockMutation.mutateAsync(userId);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("Failed to unblock user:", msg);
    }
  };

  return (
    <div
      data-testid="blocked-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <NavigableList
        items={blocked}
        isLoading={isLoading}
        emptyLabel="No blocked users."
        getKey={(b) => b.user_id}
        rowTestId={(b) => `blocked-${b.user_id}`}
        renderRow={(b) => (
          <span
            className="flex-1 truncate text-sm font-mono"
            style={{ color: "var(--c-text)" }}
          >
            {b.username ?? b.user_id}
          </span>
        )}
        controls={(b) => [
          <Button
            data-testid={`unblock-${b.user_id}`}
            onClick={() => handleUnblock(b.user_id)}
            disabled={unblockMutation.isPending}
            variant="secondary"
          >
            unblock
          </Button>,
        ]}
        trailing={(b) => <span>{timeAgo(b.blocked_at)}</span>}
      />
    </div>
  );
};
