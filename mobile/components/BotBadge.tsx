import { useTranslation } from "react-i18next";
import { Text } from "./Text";
import { Tag } from "./Tag";
import { Icon } from "./icons";
import { fonts, semantic } from "../theme/tokens";

/**
 * The Bot badge (#1182), the design-system primitive for "this member is a bot": on its row in the
 * member list, on its messages and in mentions. Driven by the roster's `is_bot` only. Decorative to
 * screen readers: the row speaks `botSpokenLabel` instead, which names who manages it.
 *
 * `inline` draws it as a text span, for inside running text (a mention in a message body).
 */
export function BotBadge({ inline = false, testID }: { inline?: boolean; testID?: string }) {
  const { t } = useTranslation("mobile");
  if (inline) {
    return (
      <Text
        testID={testID}
        accessibilityElementsHidden
        importantForAccessibility="no-hide-descendants"
        style={{ fontFamily: fonts.medium, color: semantic.dim, backgroundColor: semantic.high }}
      >
        {` ${t("bots.badge")} `}
      </Text>
    );
  }
  return (
    <Tag
      testID={testID}
      tone="neutral"
      label={t("bots.badge")}
      leading={<Icon.bot size={12} color={semantic.dim} />}
    />
  );
}
