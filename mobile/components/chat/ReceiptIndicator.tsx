import { View, Text } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";
import type { MessageReceipts } from "../../hooks/queries/useReceipts";

/**
 * Delivery/read state on an own DM message (#892). Port of desktop's
 * ReceiptIndicator state machine:
 *   - nothing until at least one delivery lands — "not delivered",
 *     "receipts off", and "not a DM" are deliberately indistinguishable,
 *     so the UI never leaks that receipts are disabled;
 *   - single check = delivered, double check = read by some (muted) /
 *     read by everyone (accent);
 *   - a count suffix appears only with more than one peer.
 */
export function ReceiptIndicator({
  receipts,
  peerCount,
  visible,
}: {
  receipts?: MessageReceipts;
  peerCount: number;
  visible: boolean;
}) {
  if (!visible || !receipts || peerCount < 1) {
    return null;
  }
  const deliveredCount = receipts.delivered_by.length;
  const readCount = receipts.read_by.length;
  if (deliveredCount === 0) {
    return null;
  }
  const allRead = readCount >= peerCount;
  const anyRead = readCount > 0;
  const color = allRead ? semantic.accent : semantic.mute;
  const label = allRead
    ? "Read by everyone"
    : anyRead
      ? `Read by ${readCount} of ${peerCount}`
      : `Delivered to ${deliveredCount} of ${peerCount}`;

  return (
    <View
      testID={`receipt-${receipts.message_id}`}
      accessibilityLabel={label}
      style={{ flexDirection: "row", alignItems: "center", gap: 3 }}
    >
      {anyRead ? (
        <Icon.checkCheck color={color} />
      ) : (
        <Icon.check color={color} />
      )}
      {peerCount > 1 ? (
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 10,
            color,
          }}
        >
          {`${anyRead ? readCount : deliveredCount}/${peerCount}`}
        </Text>
      ) : null}
    </View>
  );
}
