import { Text } from "react-native";
import { palette, semantic, t } from "../../theme/tokens";

/**
 * One resolved `@username` in a message body (port of desktop's
 * MentionToken). Self-mentions (including `@all`, which speaks to the
 * reader too) are the loud state — accent background, dark text; other
 * people's mentions are the quiet accent-on-tint state.
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
      style={
        isSelf
          ? {
              backgroundColor: semantic.accent,
              color: palette.bg,
              fontWeight: "600",
            }
          : {
              backgroundColor: t(0.12),
              color: semantic.accent,
            }
      }
    >
      @{name}
    </Text>
  );
}
