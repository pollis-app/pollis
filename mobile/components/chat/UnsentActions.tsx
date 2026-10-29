import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { Chip } from "../ui";

/** Retry / Discard under an unsent message (#1335); Retry is absent when the core never stored it. */
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
