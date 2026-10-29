import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, fonts, layout, space } from "../../theme/tokens";

const CHOICE_MIN_HEIGHT = layout.touchMin + 4;

/** One option of a `ChoiceGrid` (review6 #7); selection shows by check and radio state, never colour alone. */
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
        minHeight: CHOICE_MIN_HEIGHT,
        minWidth: layout.touchMin,
        borderRadius: CHOICE_MIN_HEIGHT / 2,
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
