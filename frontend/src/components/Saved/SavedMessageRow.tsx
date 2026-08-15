import React from "react";
import { useTranslation } from "react-i18next";
import { useSkin } from "../../hooks/queries/usePreferences";
import type { SavedMessage } from "../../hooks/queries/useBookmarks";

interface SavedMessageRowProps {
  item: SavedMessage;
  /** Human label for the conversation ("#general", "alice"), or null when this
   *  device cannot name it. */
  conversationLabel: string | null;
  /** Display name for the sender, when known. */
  senderLabel: string | null;
}

/**
 * One entry in the saved-items list.
 *
 * Two states, and the unavailable one matters as much as the normal one: a
 * bookmark deliberately outlives its message (retention eviction, soft delete),
 * and when the body is gone this row says so plainly rather than rendering an
 * empty bubble or quietly dropping out of the list.
 *
 * Both skins render the same structure and differ only in type treatment —
 * terminal keeps everything monospace and one line tall, refined uses the sans
 * stack with a little more air. The saved-items surface has no terminal-only or
 * refined-only affordances.
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
        // Honest placeholder. It says only that THIS DEVICE no longer holds the
        // message — it does not claim the message was deleted for everyone, and
        // it shows no sender, timestamp or content.
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
