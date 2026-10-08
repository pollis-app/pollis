import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, fonts, space } from "../../theme/tokens";

/**
 * One choice in a single-select list (the report reasons): a 52pt row inside
 * a <Group>, radio semantics, and the selection said by a check mark and
 * bold text as well as the accent tint — never by colour alone.
 */
export function ReasonRow({
  label,
  selected,
  onPress,
  testID,
}: {
  label: string;
  selected: boolean;
  onPress: () => void;
  testID?: string;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityLabel={label}
      accessibilityState={{ checked: selected }}
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
      <Text
        style={{
          flex: 1,
          fontFamily: selected ? fonts.bold : fonts.medium,
          fontSize: 16,
          color: selected ? semantic.accent : semantic.text,
        }}
      >
        {label}
      </Text>
      <View style={{ width: 20, alignItems: "center" }}>
        {selected ? <Icon.check size={20} color={semantic.accent} /> : null}
      </View>
    </Pressable>
  );
}
