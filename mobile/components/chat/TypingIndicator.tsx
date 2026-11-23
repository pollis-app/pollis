import { View, useWindowDimensions } from "react-native";
import { Text } from "../Text";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { semantic, type as ty } from "../../theme/tokens";
import { useThemeRerender } from "../theme";
import { typingStore } from "../../stores/typingStore";
import { describeTyping, typingRoomKey } from "../../lib/typing";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { aliasName } from "../../lib/groupAlias";

/** Cap on the line's text scaling, so the band's fixed height always holds it. */
const MAX_SCALE = 1.4;
const PAD_Y = 4;

/**
 * "X is typing…" over the composer (#1635), announced politely. The band is always there at one
 * height, empty when nobody types, so the message list never moves as the line shows or hides (#2082).
 */
export const TypingIndicator = observer(function TypingIndicator({
  channelId,
  conversationId,
  groupId,
}: {
  channelId: string | null;
  conversationId: string | null;
  /** The channel's group: typists show as it shows them (#2003). */
  groupId?: string | null;
}) {
  useThemeRerender();
  const { fontScale } = useWindowDimensions();
  const { t } = useTranslation("chat");
  const aliases = useGroupAliases(channelId ? groupId : null);
  const line = describeTyping(
    typingStore.typists(typingRoomKey(channelId, conversationId), (userId, username) =>
      aliasName(aliases, userId, username),
    ),
  );
  let text: string | null = null;
  if (line?.kind === "one") {
    text = t("typing.one", { name: line.name });
  } else if (line?.kind === "two") {
    text = t("typing.two", { first: line.first, second: line.second });
  } else if (line) {
    text = t("typing.several", { count: line.count });
  }
  const scale = Math.min(Math.max(fontScale, 1), MAX_SCALE);
  const height = Math.ceil(ty.meta.lineHeight * scale) + PAD_Y * 2;
  return (
    <View
      testID="typing-band"
      style={{ minHeight: height, paddingHorizontal: 16, paddingVertical: PAD_Y, justifyContent: "center" }}
    >
      {text ? (
        <Text
          testID="text-typing-indicator"
          accessibilityLiveRegion="polite"
          numberOfLines={1}
          maxFontSizeMultiplier={MAX_SCALE}
          style={[ty.meta, { color: semantic.muted }]}
        >
          {text}
        </Text>
      ) : null}
    </View>
  );
});
