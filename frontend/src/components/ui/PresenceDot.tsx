import React from "react";
import { observer } from "mobx-react-lite";
import { usePresenceStatus } from "../../stores/presenceStore";

interface PresenceDotProps {
  userId: string | null;
  testId?: string;
}

/**
 * Standalone presence dot mirroring Avatar's overlay dot: online uses the
 * accent color, offline uses the bg color ringed in accent-muted.
 *
 * Used wherever there is no avatar behind it to anchor the dot — the terminal
 * skin's sidebar DM list and the right panel's member rows, both of which stay
 * exactly one text-line tall.
 */
export const PresenceDot: React.FC<PresenceDotProps> = observer(
  ({ userId, testId }) => {
    const status = usePresenceStatus(userId);
    return (
      <span
        data-testid={testId}
        aria-label={`Presence: ${status}`}
        className={`inline-block size-2 rounded-full box-content shrink-0 border ${
          status === "offline"
            ? "bg-bg border-accent-muted"
            : "bg-accent border-surface"
        }`}
      />
    );
  },
);
