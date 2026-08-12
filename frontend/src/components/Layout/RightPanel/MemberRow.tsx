import React from "react";
import { observer } from "mobx-react-lite";
import { useNavigate } from "@tanstack/react-router";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { PresenceDot } from "../../ui/PresenceDot";

interface MemberRowProps {
  userId: string;
  label: string;
  avatarKey?: string | null;
  isAdmin: boolean;
}

/**
 * One person in the right panel's member list. Clicking opens their profile,
 * matching how member rows behave on the group Members page.
 *
 * Skin split mirrors the left sidebar's DM rows exactly: refined shows the
 * avatar with the presence dot anchored to it, terminal shows the bare dot so
 * every row stays one text-line tall.
 */
export const MemberRow: React.FC<MemberRowProps> = observer(
  ({ userId, label, avatarKey, isAdmin }) => {
    const navigate = useNavigate();
    const isTerminal = useSkin() === "terminal";

    const rowClass = isTerminal
      ? "flex w-full items-center gap-1.5 border-l-2 border-transparent px-2.5 py-0.5 text-left text-base text-fg transition-colors hover:bg-hover"
      : "flex w-full items-center gap-2 rounded px-2 py-1 text-left hover:bg-hover";

    return (
      <button
        type="button"
        onClick={() => navigate({ to: "/user/$userId", params: { userId } })}
        className={rowClass}
        data-testid={`right-panel-member-${userId}`}
      >
        {isTerminal ? (
          <PresenceDot
            userId={userId}
            testId={`right-panel-presence-${userId}`}
          />
        ) : (
          <PresenceAvatar
            userId={userId}
            avatarKey={avatarKey}
            size={24}
            alt={label}
            variant="list"
          />
        )}
        <span
          className={`min-w-0 flex-1 truncate ${
            isTerminal ? "" : "text-sm text-fg"
          }`}
        >
          {label}
        </span>
        {isAdmin && (
          <span
            className={`shrink-0 uppercase tracking-wide text-muted ${
              isTerminal ? "text-2xs" : "text-xs"
            }`}
          >
            admin
          </span>
        )}
      </button>
    );
  },
);
