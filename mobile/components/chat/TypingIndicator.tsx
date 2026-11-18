import { Text } from "../Text";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";
import { semantic, type as ty } from "../../theme/tokens";
import { useThemeRerender } from "../theme";
import { typingStore } from "../../stores/typingStore";
import { describeTyping, typingRoomKey } from "../../lib/typing";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { aliasName } from "../../lib/groupAlias";

/** "X is typing…" over the composer (#1635); drawn only while someone is, and announced politely. */
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
  const { t } = useTranslation("chat");
  const aliases = useGroupAliases(channelId ? groupId : null);
  const line = describeTyping(
    typingStore.typists(typingRoomKey(channelId, conversationId), (userId, username) =>
      aliasName(aliases, userId, username),
    ),
  );
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
