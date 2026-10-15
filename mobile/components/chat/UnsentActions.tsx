import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { Chip } from "../ui";

/**
 * Retry / Discard under a message that did not send (#1335) — Slack's "Not
 * delivered · Try again · Delete". Retry re-runs delivery under the same
 * message id (never a duplicate). Discard removes it — and because an earlier
 * attempt may have reached the server before the app was killed, the core
 * first withdraws it from everyone (as "delete for everyone" does) whenever it
 * was ever sealed, so no recipient keeps a copy the sender can no longer
 * delete. Retry is absent when the send failed before the core stored the
 * message: there is nothing to retry then, only to discard.
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
