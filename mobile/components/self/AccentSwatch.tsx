import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, fonts, r, space, layout } from "../../theme/tokens";

/**
 * One accent preset: a 44pt colour disc with its name under it. Selected is
 * said three ways, never by colour alone: an accent ring, a check on the disc
 * and bold text — plus the radio's checked state for screen readers. It
 * fills its grid cell (see `ChoiceGrid`) so the preset row is even.
 */
export function AccentSwatch({
  color,
  label,
  selected,
  onPress,
  testID,
  accessibilityLabel,
}: {
  color: string;
  label: string;
  selected: boolean;
  onPress: () => void;
  testID?: string;
  accessibilityLabel: string;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessibilityRole="radio"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ checked: selected, selected }}
      style={({ pressed }) => ({
        alignItems: "center",
        gap: space.xs,
        paddingVertical: space.sm,
        paddingHorizontal: 4,
        borderRadius: r.md,
        backgroundColor: pressed ? semantic.high : "transparent",
      })}
    >
      <View
        style={{
          width: layout.touchMin + 8,
          height: layout.touchMin + 8,
          borderRadius: (layout.touchMin + 8) / 2,
          borderWidth: 2,
          borderColor: selected ? semantic.text : "transparent",
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <View
          style={{
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            backgroundColor: color,
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          {selected ? <Icon.check size={22} color={semantic.onAccent} /> : null}
        </View>
      </View>
      <Text
        numberOfLines={1}
        style={[
          ty.meta,
          {
            fontSize: 13,
            fontFamily: selected ? fonts.bold : fonts.medium,
            color: selected ? semantic.text : semantic.dim,
          },
        ]}
      >
        {label}
      </Text>
    </Pressable>
  );
}
