import { Text } from "react-native";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { semantic, type as ty } from "../../theme/tokens";
import { useThemeRerender } from "../theme";
import { typingStore } from "../../stores/typingStore";
import { describeTyping, typingRoomKey } from "../../lib/typing";

/** "X is typing…" over the composer (#1635); drawn only while someone is, and announced politely. */
export const TypingIndicator = observer(function TypingIndicator({
  channelId,
  conversationId,
}: {
  channelId: string | null;
  conversationId: string | null;
}) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  const line = describeTyping(typingStore.typists(typingRoomKey(channelId, conversationId)));
  if (!line) {
    return null;
  }
  let text: string;
  if (line.kind === "one") {
    text = t("typing.one", { name: line.name });
  } else if (line.kind === "two") {
    text = t("typing.two", { first: line.first, second: line.second });
  } else {
    text = t("typing.several", { count: line.count });
  }
  return (
    <Text
      testID="text-typing-indicator"
      accessibilityLiveRegion="polite"
      numberOfLines={1}
      style={[
        ty.meta,
        { color: semantic.muted, paddingHorizontal: 16, paddingVertical: 4 },
      ]}
    >
      {text}
    </Text>
  );
});
