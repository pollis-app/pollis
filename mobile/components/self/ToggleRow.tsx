import { Pressable, Text, View } from "react-native";
import { Toggle } from "../ui";
import { semantic, type as ty, fonts, space, layout } from "../../theme/tokens";

/**
 * A settings row that IS a switch: the whole row toggles and is one control
 * for screen readers (role switch + checked state), so the visual Toggle
 * inside is not a second, nested target. `testID` (`toggle-<name>`) sits on
 * the row so e2e taps land on the control.
 */
export function ToggleRow({
  label,
  sub,
  on,
  onToggle,
  testID,
  disabled,
}: {
  label: string;
  sub?: string;
  on: boolean;
  onToggle: () => void;
  testID?: string;
  disabled?: boolean;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={disabled ? undefined : onToggle}
      disabled={disabled}
      accessibilityRole="switch"
      accessibilityLabel={sub ? `${label}, ${sub}` : label}
      accessibilityState={{ checked: on, disabled: !!disabled }}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.xl,
        minHeight: Math.max(layout.touchMin, 52),
        paddingVertical: space.sm,
        paddingStart: space.xxl,
        paddingEnd: space.xl,
        backgroundColor: pressed ? semantic.high : "transparent",
        opacity: disabled ? 0.45 : 1,
      })}
    >
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <Text style={{ fontFamily: fonts.medium, fontSize: 16, color: semantic.text }}>
          {label}
        </Text>
        {sub ? <Text style={ty.secondary}>{sub}</Text> : null}
      </View>
      <View importantForAccessibility="no-hide-descendants" accessibilityElementsHidden>
        <Toggle on={on} onPress={disabled ? undefined : onToggle} disabled={disabled} />
      </View>
    </Pressable>
  );
}
