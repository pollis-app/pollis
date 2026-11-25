import { Pressable, View } from "react-native";
import { Icon } from "../icons";
import { semantic, r, space, layout } from "../../theme/tokens";

/**
 * The card shell of an entry at the top of the Direct list (the Vault, message requests): the
 * search field's corner and fill (#2101), a glyph column, the body, and a chevron.
 */
export function DirectHeadRow({
  glyph,
  children,
  onPress,
  accessibilityLabel,
  testID,
}: {
  glyph: React.ReactNode;
  children: React.ReactNode;
  onPress: () => void;
  accessibilityLabel: string;
  testID?: string;
}) {
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessible
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 52,
        paddingVertical: space.sm,
        paddingHorizontal: space.lg,
        borderRadius: r.box,
        backgroundColor: pressed ? semantic.high : semantic.raised,
      })}
    >
      <View style={{ width: layout.touchMin, alignItems: "center" }}>{glyph}</View>
      {children}
      <Icon.chevronRight size={18} color={semantic.muted} />
    </Pressable>
  );
}
