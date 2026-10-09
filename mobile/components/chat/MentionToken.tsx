import { Text } from "react-native";
import { fonts, semantic } from "../../theme/tokens";

/**
 * One resolved `@username` in a message body (port of desktop's
 * MentionToken). Accent text on an accent-tint pill. Self-mentions
 * (including `@all`, which speaks to the reader too) take the stronger tint
 * and weight; the whole row is also tinted by MessageRow, so the state is
 * never carried by colour alone.
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
