import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Text } from "../Text";
import { Icon } from "../icons";
import { semantic, type as ty, space, r } from "../../theme/tokens";
import type { AppNotice } from "../../lib/botApps";

/**
 * The admin's notice that a bot is not running (#2005): waiting for an approval, or refused because
 * its approval or build does not check out. A solid block with an edge, never a glow; the wording
 * and the icon tell the two apart, not a hue alone.
 */
export function AppNoticeBlock({ notice }: { notice: AppNotice }) {
  const { t } = useTranslation("mobile");
  const text = notice.waiting
    ? t("bots.app.waiting")
    : t("bots.app.notRunning", { reason: notice.reasonKey ? t(notice.reasonKey) : notice.detail });
  return (
    <View
      testID={notice.waiting ? "notice-bot-app-waiting" : "notice-bot-app-refused"}
      accessible
      accessibilityRole="alert"
      style={{
        flexDirection: "row",
        alignItems: "flex-start",
        gap: space.md,
        padding: space.xxl,
        borderRadius: r.lg,
        borderWidth: 1,
        borderColor: notice.waiting ? semantic.edge : semantic.danger,
        backgroundColor: semantic.raised,
      }}
    >
      <View style={{ paddingTop: 2 }}>
        {notice.waiting ? (
          <Icon.clock size={18} color={semantic.dim} />
        ) : (
          <Icon.alert size={18} color={semantic.danger} />
        )}
      </View>
      <Text style={[ty.body, { flex: 1, color: semantic.text }]}>{text}</Text>
    </View>
  );
}
