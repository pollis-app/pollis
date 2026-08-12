import React from "react";
import { observer } from "mobx-react-lite";
import { useNavigate } from "@tanstack/react-router";
import { PresenceAvatar } from "../../ui/PresenceAvatar";

interface MemberRowProps {
  userId: string;
  label: string;
  avatarKey?: string | null;
  isAdmin: boolean;
}

/**
 * One person in the right panel's member list. Clicking opens their profile,
 * matching how member rows behave on the group Members page.
 */
export const MemberRow: React.FC<MemberRowProps> = observer(
  ({ userId, label, avatarKey, isAdmin }) => {
    const navigate = useNavigate();

    return (
      <button
        type="button"
        onClick={() => navigate({ to: "/user/$userId", params: { userId } })}
        className="flex w-full items-center gap-2 rounded px-2 py-1 text-left hover:bg-hover"
        data-testid={`right-panel-member-${userId}`}
      >
        <PresenceAvatar
          userId={userId}
          avatarKey={avatarKey}
          size={24}
          alt={label}
          variant="list"
        />
        <span className="min-w-0 flex-1 truncate text-sm text-fg">{label}</span>
        {isAdmin && (
          <span className="shrink-0 text-xs uppercase tracking-wide text-muted">
            admin
          </span>
        )}
      </button>
    );
  },
);
