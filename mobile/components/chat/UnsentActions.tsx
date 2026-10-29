import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { Chip } from "../ui";

/**
 * Retry / Discard under a message that did not send (#1335). Retry reuses the
 * message id (never a duplicate). Discard first withdraws the message from
 * everyone if it was ever sealed, since an earlier attempt may have reached
 * the server. Retry is absent when the core never stored the message.
 */
export function UnsentActions({
  messageId,
  onRetry,
  onDiscard,
}: {
  messageId: string;
  onRetry?: () => void;
  onDiscard?: () => void;
}) {
  const { t } = useTranslation("chat");
  return (
    <View style={{ flexDirection: "row", alignItems: "center", gap: 8 }}>
      {onRetry ? (
        <Chip
          variant="solid"
          onPress={onRetry}
          testID={`btn-retry-message-${messageId}`}
          accessibilityLabel={t("outbox.retryLabel")}
        >
          {t("outbox.retry")}
        </Chip>
      ) : null}
      {onDiscard ? (
        <Chip
          onPress={onDiscard}
          testID={`btn-discard-message-${messageId}`}
          accessibilityLabel={t("outbox.discardLabel")}
        >
          {t("outbox.discard")}
        </Chip>
      ) : null}
    </View>
  );
}
