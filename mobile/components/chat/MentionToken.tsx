import { Text } from "react-native";
import { fonts, semantic } from "../../theme/tokens";

/**
 * One resolved `@username` in a message body. Self-mentions (including
 * `@all`) take the stronger tint and weight; MessageRow also tints the whole
 * row, so the state is never colour alone.
 */
export function MentionToken({
  name,
  isSelf,
}: {
  name: string;
  isSelf: boolean;
}) {
  return (
    <Text
      testID={isSelf ? "mention-self" : "mention-other"}
      style={{
        fontFamily: isSelf ? fonts.semibold : fonts.medium,
        backgroundColor: isSelf ? semantic.accentMid : semantic.accentSoft,
        color: semantic.accent,
      }}
    >
      @{name}
    </Text>
  );
}
