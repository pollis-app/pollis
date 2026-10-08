import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, fonts, layout, space } from "../../theme/tokens";

/**
 * One option of a single-choice set laid out in a `ChoiceGrid`: fills its
 * cell, at least 44pt tall so the visible shape is the whole hit area. Unselected
 * options carry an outline so each reads as tappable on any surface;
 * selected is said by a check, the accentSoft fill + accentLine border, the
 * accent label and the radio's checked state — never by colour alone. The
 * label wraps rather than truncates at large text sizes.
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
        borderWidth: 1,
        borderColor: selected ? semantic.accentLine : semantic.edge,
        backgroundColor: selected
          ? semantic.accentSoft
          : pressed
            ? semantic.high
            : "transparent",
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
        {selected ? <Icon.check size={16} color={semantic.accent} /> : null}
        <Text
          style={{
            flexShrink: 1,
            textAlign: "center",
            fontFamily: fonts.semibold,
            fontSize: 14,
            color: selected ? semantic.accent : semantic.text,
          }}
        >
          {label}
        </Text>
      </View>
    </Pressable>
  );
}
