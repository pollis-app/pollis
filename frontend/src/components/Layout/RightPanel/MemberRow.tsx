import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNavigate } from "@tanstack/react-router";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { PresenceAvatar } from "../../ui/PresenceAvatar";
import { PresenceDot } from "../../ui/PresenceDot";
import { PressButton } from "../../ui/PressButton";

interface MemberRowProps {
  userId: string;
  label: string;
  avatarKey?: string | null;
  isAdmin: boolean;
}

/** One member; opens their profile. Modern shows the avatar, terminal a bare dot. */
export const MemberRow: React.FC<MemberRowProps> = observer(
  ({ userId, label, avatarKey, isAdmin }) => {
    const { t } = useTranslation("nav");
    const navigate = useNavigate();
    const isTerminal = useSkin() === "terminal";

    const rowClass = isTerminal
      ? "flex w-full items-center gap-1.5 border-s-2 border-transparent px-2.5 py-0.5 text-start text-row text-fg hover:bg-hover"
      : "flex w-full items-center gap-2 rounded-control px-2 py-1 text-start hover:bg-hover";

    return (
      <PressButton
        activateOn="click"
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
        <bdi
          className={`min-w-0 flex-1 truncate ${
            isTerminal ? "" : "text-body text-fg"
          }`}
        >
          {label}
        </bdi>
        {isAdmin && (
          <span
            className={`shrink-0 uppercase tracking-wide text-muted ${
              isTerminal ? "text-meta" : "text-secondary"
            }`}
          >
            {t("members.admin")}
          </span>
        )}
      </PressButton>
    );
  },
);
