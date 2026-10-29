import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { usePresenceStatus } from "../../stores/presenceStore";

interface PresenceDotProps {
  userId: string | null;
  testId?: string;
}

/** Standalone presence dot for rows with no avatar (terminal DM list, member rows). */
export const PresenceDot: React.FC<PresenceDotProps> = observer(
  ({ userId, testId }) => {
    const { t } = useTranslation("common");
    const status = usePresenceStatus(userId);
    return (
      <span
        data-testid={testId}
        aria-label={t("presence.label", { status: t(`presence.${status}`) })}
        className={`inline-block size-2 rounded-full box-content shrink-0 border ${
          status === "offline"
            ? "bg-bg border-accent-muted"
            : "bg-accent border-surface"
        }`}
      />
    );
  },
);
