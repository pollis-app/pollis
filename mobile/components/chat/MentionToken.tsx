import { Text } from "react-native";
import { fonts, semantic } from "../../theme/tokens";

/** One resolved `@username` in a message body; self-mentions take the stronger tint. */
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
