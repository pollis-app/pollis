import { Pressable, Text, View } from "react-native";
import { Icon } from "./icons";
import { semantic, type as ty, fonts, space } from "../theme/tokens";

/**
 * One full-width single-select row, for a radio list inside a `Group` (wrap the list in an
 * `accessibilityRole="radiogroup"` view). The selected row is tinted, bold and carries a check:
 * selection is never colour alone.
 */
export function RadioRow({
  label,
  sub,
  selected,
  onPress,
  testID,
}: {
  label: string;
  sub?: string;
  selected: boolean;
  onPress: () => void;
  testID?: string;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityLabel={sub ? `${label}, ${sub}` : label}
      accessibilityState={{ checked: selected, selected }}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 52,
        paddingVertical: space.sm,
        paddingStart: space.xxl,
        paddingEnd: space.xl,
        backgroundColor: selected
          ? semantic.accentSoft
          : pressed
            ? semantic.high
            : "transparent",
      })}
    >
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <Text
          style={{
            fontFamily: selected ? fonts.bold : fonts.medium,
            fontSize: 16,
            color: selected ? semantic.accent : semantic.text,
          }}
        >
          {label}
        </Text>
        {sub ? <Text style={ty.secondary}>{sub}</Text> : null}
      </View>
      <View style={{ width: 20, alignItems: "center" }}>
        {selected ? <Icon.check size={20} color={semantic.accent} /> : null}
      </View>
    </Pressable>
  );
}
