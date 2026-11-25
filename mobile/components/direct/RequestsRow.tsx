import { Text } from "../Text";
import { useTranslation } from "react-i18next";
import { Badge } from "../ui";
import { Icon } from "../icons";
import { semantic, fonts, typeSize } from "../../theme/tokens";
import { DirectHeadRow } from "./DirectHeadRow";

/** "Message requests" entry at the top of the Direct list, shown only when there are any. */
export function RequestsRow({
  count,
  onPress,
  testID,
}: {
  count: number;
  onPress: () => void;
  testID?: string;
}) {
  const { t } = useTranslation("mobile");
  return (
    <DirectHeadRow
      testID={testID}
      onPress={onPress}
      accessibilityLabel={t("direct.requestsWaiting", { count })}
      glyph={<Icon.mail size={20} color={semantic.dim} />}
    >
      <Text
        numberOfLines={2}
        style={{
          flex: 1,
          fontFamily: fonts.semibold,
          fontSize: typeSize.button,
          color: semantic.text,
        }}
      >
        {t("direct.requests")}
      </Text>
      <Badge tone="neutral">{count > 99 ? "99+" : count}</Badge>
    </DirectHeadRow>
  );
}
