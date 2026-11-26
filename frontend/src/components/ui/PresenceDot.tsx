import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { usePresenceStatus } from "../../stores/presenceStore";

interface PresenceDotProps {
  userId: string | null;
  testId?: string;
}

/** Standalone presence dot, for rows with no avatar to anchor it: online, away (a crescent), offline. */
export const PresenceDot: React.FC<PresenceDotProps> = observer(
  ({ userId, testId }) => {
    const { t } = useTranslation("common");
    const status = usePresenceStatus(userId);
    return (
      <span
        data-testid={testId}
        data-presence={status}
        aria-label={t("presence.label", { status: t(`presence.${status}`) })}
        className={`relative inline-block size-2 overflow-hidden rounded-full box-content shrink-0 border ${
          status === "offline"
            ? "bg-bg border-accent-muted"
            : "bg-accent border-surface"
        }`}
      >
        {/* #2110: away bites the dot into a crescent, in the ring's colour. */}
        {status === "away" ? (
          <span aria-hidden="true" className="absolute -left-[20%] -top-[20%] size-[80%] rounded-full bg-surface" />
        ) : null}
      </span>
    );
  },
);
