import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../hooks/queries/usePreferences";
import type { SavedMessage } from "../../hooks/queries/useBookmarks";

interface SavedMessageRowProps {
  item: SavedMessage;
  /** "#general", "alice", or null when this device can't name it. */
  conversationLabel: string | null;
  senderLabel: string | null;
}

/**
 * One saved item. A bookmark outlives its message (retention, soft delete), so
 * the unavailable state says so plainly rather than dropping the row.
 */
export const SavedMessageRow: React.FC<SavedMessageRowProps> = ({
  item,
  conversationLabel,
  senderLabel,
}) => {
  const { t } = useTranslation("saved");
  const isTerminal = useSkin() === "terminal";

  const metaClass = isTerminal
    ? "text-2xs font-mono text-muted"
    : "text-xs text-muted";
  const bodyClass = isTerminal
    ? "text-xs font-mono text-fg truncate"
    : "text-sm text-fg truncate";

  return (
    <div className="flex min-w-0 flex-1 flex-col gap-0.5">
      <div className="flex min-w-0 items-center gap-2">
        <span className={`${metaClass} shrink-0 text-accent`}>
          {conversationLabel ?? t("row.conversationFallback")}
        </span>
        {senderLabel && (
          <span className={`${metaClass} min-w-0 truncate`}>{senderLabel}</span>
        )}
      </div>

      {item.available ? (
        <span data-testid={`saved-content-${item.message_id}`} className={bodyClass}>
          {item.content && item.content.length > 0 ? item.content : t("row.noText")}
        </span>
      ) : (
        // Says only that THIS DEVICE no longer holds it: no sender, time or content.
        <span
          data-testid={`saved-unavailable-${item.message_id}`}
          className={`${bodyClass} italic text-dim`}
        >
          {t("row.unavailable")}
        </span>
      )}
    </div>
  );
};
