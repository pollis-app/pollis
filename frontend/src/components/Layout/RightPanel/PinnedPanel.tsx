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
import { EmptyState } from "../../ui/EmptyState";
import { SectionHeader } from "../../ui/SectionHeader";
import { IconButton } from "../../ui/IconButton";

interface PinnedPanelProps {
  channelId: string | null;
  conversationId: string | null;
}

/** Pinned messages (#99) in the right panel. A missing pin key (fresh external join) shows as "not yet available". */
export const PinnedPanel: React.FC<PinnedPanelProps> = observer(
  ({ channelId, conversationId }) => {
    const { t } = useTranslation("nav");
    const isTerminal = useSkin() !== "modern";
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
        <EmptyState background={false} testId="pinned-panel-unavailable">
          {t("pins.unavailable")}
        </EmptyState>
      );
    } else if (pins.length === 0) {
      body = (
        <EmptyState background={false} testId="pinned-panel-empty">
          {t("pins.empty")}
        </EmptyState>
      );
    } else {
      body = (
        <ul className="flex flex-col">
          {pins.map((pin) => {
            const parsed = parseContent(pin.content, { conversationId: pin.conversation_id });
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
                      isTerminal ? "text-body" : "text-secondary"
                    }`}
                  >
                    {author}
                  </span>
                  <span className="shrink-0 text-meta text-muted">
                    {formatClockTime(new Date(pin.sent_at).getTime())}
                  </span>
                  <IconButton
                    className="shrink-0 opacity-0 transition-opacity focus-visible:opacity-100 group-hover:opacity-100"
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
                  </IconButton>
                </div>
                {parsed.text ? (
                  <p
                    className={`mt-0.5 whitespace-pre-wrap break-words text-fg ${
                      isTerminal ? "text-row" : "text-body"
                    }`}
                  >
                    {parsed.text}
                  </p>
                ) : null}
                {parsed.attachments && parsed.attachments.length > 0 ? (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {parsed.attachments.map((attachment) => (
                      <AttachmentDisplay key={attachment.id} attachment={attachment} roll={parsed.attachments} />
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
        <SectionHeader>
          <Pin size={12} aria-hidden="true" />
          {t("pins.heading")}
        </SectionHeader>
        {body}
      </div>
    );
  },
);
