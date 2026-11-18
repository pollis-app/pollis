import { Text } from "../Text";
import { BotBadge } from "../BotBadge";
import { fonts, semantic } from "../../theme/tokens";

/**
 * One resolved `@username` in a message body; self-mentions take the stronger tint.
 * `label`: the handle's alias in this group (#2003), shown in place of the handle.
 * `isBot`: the mentioned member is a bot (#1182), so the Bot badge follows the name.
 */
export function MentionToken({
  name,
  label,
  isSelf,
  isBot = false,
}: {
  name: string;
  label?: string;
  isSelf: boolean;
  isBot?: boolean;
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
      {isBot ? <BotBadge inline testID="mention-bot-badge" /> : null}
    </Text>
  );
}
