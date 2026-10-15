import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { useDiscardUnsent, useRetryMessage } from "../../hooks/queries/useMessages";
import { retryTarget } from "../../utils/pendingMessages";
import type { Message } from "../../types";

interface UnsentMessageActionsProps {
  message: Message;
}

/**
 * Retry / Discard beside a message that did not send (#1335) — Slack's "Not
 * delivered · Try again · Delete". Retry re-runs delivery under the same
 * message id, so the message can never arrive twice; Discard throws the unsent
 * copy away, which reaches nobody because nothing was delivered. Retry is
 * absent when the send failed before the core stored the message (an
 * attachment upload): there is nothing to retry then, only to discard.
 *
 * Rendered by both skins' `MessageItem` and by the thread panel's row, so a
 * failed thread reply gets the same way out as a failed channel message.
 */
export const UnsentMessageActions: React.FC<UnsentMessageActionsProps> = ({ message }) => {
  const { t } = useTranslation("chat");
  const retry = useRetryMessage();
  const discard = useDiscardUnsent();
  const retryId = retryTarget(message);
  const discardId = message.outbox_id ?? message.id;

  return (
    <span className="ms-2 inline-flex items-center gap-1 align-middle">
      {retryId && (
        <Button
          data-testid={`message-retry-${message.id}`}
          variant="ghost"
          size="xs"
          aria-label={t("outbox.retryLabel")}
          isLoading={retry.isPending}
          disabled={retry.isPending || discard.isPending}
          onClick={() => retry.mutate(retryId)}
        >
          {t("outbox.retry")}
        </Button>
      )}
      <Button
        data-testid={`message-discard-${message.id}`}
        variant="ghost"
        size="xs"
        aria-label={t("outbox.discardLabel")}
        disabled={retry.isPending || discard.isPending}
        onClick={() => discard.mutate(discardId)}
      >
        {t("outbox.discard")}
      </Button>
    </span>
  );
};
