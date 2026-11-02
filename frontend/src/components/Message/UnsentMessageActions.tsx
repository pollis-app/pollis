import React from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { useDiscardUnsent, useRetryMessage } from "../../hooks/queries/useMessages";
import { retryTarget, unsentReason } from "../../utils/pendingMessages";
import type { Message } from "../../types";

interface UnsentMessageActionsProps {
  message: Message;
}

/** "Not sent", why, then Retry (same message id, never twice) / Discard (withdrawn from everyone if ever sealed) for an unsent message (#1335, #1668). */
export const UnsentMessageActions: React.FC<UnsentMessageActionsProps> = ({ message }) => {
  const { t } = useTranslation("chat");
  const retry = useRetryMessage();
  const discard = useDiscardUnsent();
  const retryId = retryTarget(message);
  const discardId = message.outbox_id ?? message.id;
  const reason = unsentReason(message) ?? "willResend";

  return (
    <span className="ms-1 inline-flex flex-wrap items-center gap-1 align-middle">
      <span className="text-xs font-machine text-muted">[{t("outbox.notSent")}]</span>
      <span className="text-xs text-muted" data-testid={`message-unsent-reason-${message.id}`}>
        {reason === "willResend" ? t("outbox.willResend") : t("outbox.notDelivered")}
      </span>
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
