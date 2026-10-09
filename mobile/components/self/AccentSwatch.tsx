import { Pressable, Text, View } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, fonts, r, space, layout } from "../../theme/tokens";

/**
 * One accent preset: a colour disc with its name under it. No ring: selected
 * is said by a dark check on the disc, a larger disc (52pt against 44), a
 * filled `high` backing tile behind the whole cell (it sits in a raised
 * Group) and bold text — plus the radio's checked state for screen readers.
 * It fills its grid cell (see `ChoiceGrid`) so the preset row is even.
 */
const DISC = layout.touchMin;
const DISC_SELECTED = layout.touchMin + 8;
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
        backgroundColor: selected || pressed ? semantic.high : "transparent",
      })}
    >
      {/* A fixed box at the selected size, so the row does not shift. */}
      <View
        style={{
          width: DISC_SELECTED,
          height: DISC_SELECTED,
          alignItems: "center",
          justifyContent: "center",
        }}
      >
        <View
          style={{
            width: selected ? DISC_SELECTED : DISC,
            height: selected ? DISC_SELECTED : DISC,
            borderRadius: (selected ? DISC_SELECTED : DISC) / 2,
            backgroundColor: color,
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          {selected ? <Icon.check size={24} color={semantic.onAccent} /> : null}
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
