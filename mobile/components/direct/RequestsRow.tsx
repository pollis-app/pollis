import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Badge } from "../ui";
import { Icon } from "../icons";
import { semantic, fonts, r, space, layout } from "../../theme/tokens";

/**
 * "Message requests" entry at the top of the Direct list (Messages.dc.html):
 * one 52pt panel row with the waiting count, opening the requests screen.
 * Rendered only when there is at least one request.
 */
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
    <Pressable
      testID={testID}
      onPress={onPress}
      accessible
      accessibilityRole="button"
      accessibilityLabel={t("direct.requestsWaiting", { count })}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 52,
        paddingVertical: space.sm,
        paddingHorizontal: space.sm,
        borderRadius: r.md,
        backgroundColor: pressed ? semantic.raised : semantic.panel,
      })}
    >
      <View style={{ width: layout.touchMin, alignItems: "center" }}>
        <Icon.mail size={20} color={semantic.dim} />
      </View>
      <Text
        numberOfLines={2}
        style={{
          flex: 1,
          fontFamily: fonts.semibold,
          fontSize: 15,
          color: semantic.text,
        }}
      >
        {t("direct.requests")}
      </Text>
      <Badge tone="neutral">{count > 99 ? "99+" : count}</Badge>
      <Icon.chevronRight size={18} color={semantic.muted} />
    </Pressable>
  );
}
