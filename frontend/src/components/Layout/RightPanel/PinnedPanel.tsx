import React from "react";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Pin, PinOff } from "lucide-react";
import { usePinnedMessages, useTogglePinnedMessage } from "../../../hooks/queries/usePins";
import { parseContent } from "../../../hooks/queries/useMessages";
import { useSkin } from "../../../hooks/queries/usePreferences";
import { AttachmentDisplay } from "../../Message/AttachmentDisplay";
import { LoadingSpinner } from "../../ui/LoaderSpinner";
import { formatClockTime } from "../../../utils/format";

interface PinnedPanelProps {
  channelId: string | null;
  conversationId: string | null;
}

/**
 * The conversation's pinned messages (#99), as a right-panel shape — the same
 * slot the thread panel uses, because both are "a focused subset of this
 * conversation" (Slack renders pins exactly here).
 *
 * Rows are deliberately panel-shaped rather than a `MessageItem` reuse, for
 * the reasons `ThreadMessageRow` documents; the one affordance a pin row
 * carries is unpin, which any member may use.
 *
 * The one non-generic state worth naming: on a device that just external-
 * joined, the pin KEY may not be unwrappable yet (it heals when any existing
 * member syncs). That surfaces as the query's error state, worded as
 * "not yet available" rather than failure.
 */
export const PinnedPanel: React.FC<PinnedPanelProps> = observer(
  ({ channelId, conversationId }) => {
    const { t } = useTranslation("nav");
    const isTerminal = useSkin() !== "refined";
    const activeConversationId = channelId ?? conversationId;
    const { data: pins = [], isLoading, isError } = usePinnedMessages(activeConversationId);
    const togglePin = useTogglePinnedMessage();

    const gutter = isTerminal ? "px-2.5" : "px-2";

    let body: React.ReactNode;
    if (isLoading) {
      body = (
        <div className="flex justify-center py-6">
          <LoadingSpinner />
        </div>
      );
    } else if (isError) {
      body = (
        <p className={`${gutter} py-2 text-xs text-muted`} data-testid="pinned-panel-unavailable">
          {t("pins.unavailable")}
        </p>
      );
    } else if (pins.length === 0) {
      body = (
        <p className={`${gutter} py-2 text-xs text-muted`} data-testid="pinned-panel-empty">
          {t("pins.empty")}
        </p>
      );
    } else {
      body = (
        <ul className="flex flex-col">
          {pins.map((pin) => {
            const parsed = parseContent(pin.content);
            const author = pin.sender_username ?? pin.sender_id;
            return (
              <li
                key={pin.id}
                className={`group border-b border-line ${gutter} py-2`}
                data-testid="pinned-panel-row"
              >
                <div className="flex items-baseline gap-2">
                  <span
                    className={`min-w-0 flex-1 truncate font-medium text-fg ${
                      isTerminal ? "text-sm" : "text-xs"
                    }`}
                  >
                    {author}
                  </span>
                  <span className="shrink-0 text-2xs text-muted">
                    {formatClockTime(new Date(pin.sent_at).getTime())}
                  </span>
                  <button
                    type="button"
                    className="icon-btn-sm shrink-0 opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100"
                    aria-label={t("pins.unpin")}
                    title={t("pins.unpin")}
                    data-testid="pinned-panel-unpin"
                    onClick={() =>
                      togglePin.mutate({
                        conversationId: pin.conversation_id,
                        messageId: pin.message_id,
                        isPinned: true,
                      })
                    }
                  >
                    <PinOff size={14} aria-hidden="true" />
                  </button>
                </div>
                {parsed.text ? (
                  <p
                    className={`mt-0.5 whitespace-pre-wrap break-words text-fg ${
                      isTerminal ? "text-base" : "text-sm"
                    }`}
                  >
                    {parsed.text}
                  </p>
                ) : null}
                {parsed.attachments && parsed.attachments.length > 0 ? (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {parsed.attachments.map((attachment) => (
                      <AttachmentDisplay key={attachment.id} attachment={attachment} />
                    ))}
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      );
    }

    return (
      <div className="h-full overflow-y-auto" data-testid="pinned-panel">
        <div
          className={
            isTerminal
              ? "sticky top-0 z-[1] flex h-bar w-full items-center gap-2 border-b border-line bg-surface px-2.5 text-[0.8rem] uppercase tracking-[0.08em] text-muted select-none"
              : "flex items-center gap-2 px-2 pt-3 pb-1 text-xs font-medium uppercase tracking-widest text-muted"
          }
        >
          <Pin size={12} aria-hidden="true" />
          {t("pins.heading")}
        </div>
        {body}
      </div>
    );
  },
);
