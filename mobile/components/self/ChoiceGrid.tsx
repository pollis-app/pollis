import { Children, type ReactNode } from "react";
import { View, type StyleProp, type ViewStyle } from "react-native";
import { space } from "../../theme/tokens";

/**
 * Equal-width columns, row by row. Unlike a flexWrap row it does not depend on
 * how wide each platform renders a label, so iOS and Android show the same
 * shape; a short last row is padded with empty cells.
 */
export function ChoiceGrid({
  columns,
  children,
  gap = space.sm,
  style,
  accessibilityRole,
  accessibilityLabel,
}: {
  columns: number;
  children: ReactNode;
  gap?: number;
  style?: StyleProp<ViewStyle>;
  accessibilityRole?: "radiogroup";
  accessibilityLabel?: string;
}) {
  const items = Children.toArray(children);
  const cols = Math.max(1, columns);
  const rows: ReactNode[][] = [];
  for (let i = 0; i < items.length; i += cols) {
    rows.push(items.slice(i, i + cols));
  }
  return (
    <View
      accessibilityRole={accessibilityRole}
      accessibilityLabel={accessibilityLabel}
      style={[{ gap }, style]}
    >
      {rows.map((row, ri) => (
        <View key={ri} style={{ flexDirection: "row", gap }}>
          {Array.from({ length: cols }, (_, ci) => (
            <View key={ci} style={{ flex: 1, minWidth: 0 }}>
              {row[ci] ?? null}
            </View>
          ))}
        </View>
      ))}
    </View>
  );
}
