import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { Chip } from "../ui";
import { semantic, type as ty } from "../../theme/tokens";
import type { UnsentReason } from "../../hooks/queries/pendingMessages";

/** Why an unsent message is unsent, then Retry / Discard (#1335); Retry is absent when the core never stored it. */
export function UnsentActions({
  messageId,
  reason,
  onRetry,
  onDiscard,
}: {
  messageId: string;
  reason: UnsentReason;
  onRetry?: () => void;
  onDiscard?: () => void;
}) {
  const { t } = useTranslation("chat");
  return (
    <View style={{ gap: 6, paddingTop: 2 }}>
      <Text
        style={[ty.meta, { color: semantic.muted }]}
        testID={`text-unsent-reason-${messageId}`}
      >
        {reason === "willResend"
          ? t("outbox.willResend")
          : t("outbox.notDelivered")}
      </Text>
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
    </View>
  );
}
