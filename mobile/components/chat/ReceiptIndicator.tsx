import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";

type T = (key: string, opts?: Record<string, unknown>) => string;

/** What the tick says, or null when it draws nothing; also read into the row's spoken label (#1637). */
export function receiptLabel(
  t: T,
  receipts: MessageReceipts | undefined,
  peerCount: number,
): string | null {
  if (!receipts || peerCount < 1) {
    return null;
  }
  const deliveredCount = receipts.delivered_by.length;
  const readCount = receipts.read_by.length;
  if (deliveredCount === 0) {
    return null;
  }
  if (readCount >= peerCount) {
    return t("receipts.readByEveryone");
  }
  if (readCount > 0) {
    return t("receipts.readByCount", { count: readCount, total: peerCount });
  }
  return t("receipts.deliveredCount", { count: deliveredCount, total: peerCount });
}

/** Delivery/read state on an own DM message (#892): nothing until a delivery lands, so "receipts off" never leaks. */
export function ReceiptIndicator({
  receipts,
  peerCount,
  visible,
}: {
  receipts?: MessageReceipts;
  peerCount: number;
  visible: boolean;
}) {
  const { t } = useTranslation("chat");
  const label = visible ? receiptLabel(t, receipts, peerCount) : null;
  if (!label || !receipts) {
    return null;
  }
  const deliveredCount = receipts.delivered_by.length;
  const readCount = receipts.read_by.length;
  const allRead = readCount >= peerCount;
  const anyRead = readCount > 0;
  const color = allRead ? semantic.accent : semantic.muted;

  return (
    <View
      testID={`receipt-${receipts.message_id}`}
      accessibilityLabel={label}
      style={{ flexDirection: "row", alignItems: "center", gap: 3 }}
    >
      {anyRead ? (
        <Icon.checkCheck size={16} color={color} />
      ) : (
        <Icon.check size={16} color={color} />
      )}
      {peerCount > 1 ? (
        <Text
          style={[ty.meta, { color }]}
        >
          {`${anyRead ? readCount : deliveredCount}/${peerCount}`}
        </Text>
      ) : null}
    </View>
  );
}
