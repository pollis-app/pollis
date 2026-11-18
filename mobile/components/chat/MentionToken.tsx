import { Text } from "../Text";
import { fonts, semantic } from "../../theme/tokens";

/**
 * One resolved `@username` in a message body; self-mentions take the stronger tint.
 * `label`: the handle's alias in this group (#2003), shown in place of the handle.
 */
export function MentionToken({
  name,
  label,
  isSelf,
}: {
  name: string;
  label?: string;
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
      @{label ?? name}
    </Text>
  );
}
