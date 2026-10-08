import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, fonts, layout, space } from "../../theme/tokens";

/**
 * One option of a single-choice set laid out in a `ChoiceGrid`: fills its
 * cell, at least 44pt tall so the visible shape is the whole hit area. No
 * border: it sits in a raised Group, so unselected options are a `high` fill
 * with an accent label (the shape says "tappable"); selected is the inverse,
 * an accent fill with a dark onAccent check and label, plus the radio's
 * checked state — never by colour alone. The label wraps rather than
 * truncates at large text sizes.
 */
export function ChoiceChip({
  label,
  selected,
  onPress,
  testID,
  accessibilityLabel,
}: {
  label: string;
  selected: boolean;
  onPress: () => void;
  testID?: string;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityLabel={accessibilityLabel ?? label}
      accessibilityState={{ checked: selected, selected }}
      style={({ pressed }) => ({
        minHeight: layout.touchMin,
        borderRadius: layout.touchMin / 2,
        backgroundColor: selected
          ? semantic.accent
          : pressed
            ? semantic.accentSoft
            : semantic.high,
        paddingHorizontal: space.sm,
        paddingVertical: space.xs,
        justifyContent: "center",
      })}
    >
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "center",
          gap: space.xs,
        }}
      >
        {selected ? <Icon.check size={16} color={semantic.onAccent} /> : null}
        <Text
          style={{
            flexShrink: 1,
            textAlign: "center",
            fontFamily: fonts.semibold,
            fontSize: 14,
            color: selected ? semantic.onAccent : semantic.accent,
          }}
        >
          {label}
        </Text>
      </View>
    </Pressable>
  );
}
