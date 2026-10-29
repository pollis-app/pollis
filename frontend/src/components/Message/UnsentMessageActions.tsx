import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { useDiscardUnsent, useRetryMessage } from "../../hooks/queries/useMessages";
import { retryTarget } from "../../utils/pendingMessages";
import type { Message } from "../../types";

interface UnsentMessageActionsProps {
  message: Message;
}

/** Retry (same message id, never twice) / Discard (withdrawn from everyone if ever sealed) for an unsent message (#1335). */
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
