import Animated, { FadeOut } from "react-native-reanimated";
import { useTranslation } from "react-i18next";
import { Chip } from "../ui";
import { Icon } from "../icons";
import { semantic, space } from "../../theme/tokens";
import { MOTION, REDUCED_FADE_IN, messageRiseIn } from "../../theme/motion";
import { useReduceMotion } from "../../hooks/useReduceMotion";
import { useThemeRerender } from "../theme";

const RISE = messageRiseIn(false);
const LEAVE = FadeOut.duration(MOTION.fastMs);

/**
 * "N new messages" (#2125): shown over the bottom of the log while the reader is scrolled up and
 * others' messages land under them; a tap goes to the newest. Not a reaction pill.
 */
export function NewMessagesButton({ count, onPress }: { count: number; onPress: () => void }) {
  useThemeRerender();
  const { t } = useTranslation("chat");
  const reduceMotion = useReduceMotion();
  const label = t("list.newMessages", { count });
  return (
    <Animated.View
      entering={reduceMotion ? REDUCED_FADE_IN : RISE}
      exiting={LEAVE}
      pointerEvents="box-none"
      style={{ position: "absolute", start: 0, end: 0, bottom: space.sm, alignItems: "center" }}
    >
      <Chip
        testID="new-messages-button"
        variant="solid"
        pressIn
        onPress={onPress}
        accessibilityLabel={`${label}. ${t("list.newMessagesHint")}`}
        leading={<Icon.chevronDown size={16} color={semantic.onAccent} />}
      >
        {label}
      </Chip>
    </Animated.View>
  );
}
