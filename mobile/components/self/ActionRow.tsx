import { Text, View } from "react-native";
import { semantic, type as ty, fonts, space, layout } from "../../theme/tokens";

/**
 * A grouped-list row that carries its own button (Unblock, Revoke…). Unlike
 * ListRow the row itself is not one accessible element — that would swallow
 * the button — so the text block is one element with the full spoken label
 * and the action is a second, separately focusable control.
 */
export function ActionRow({
  glyph,
  name,
  sub,
  action,
  testID,
  accessibilityLabel,
  minHeight = 64,
}: {
  glyph?: React.ReactNode;
  name: string;
  sub?: string;
  action?: React.ReactNode;
  testID?: string;
  accessibilityLabel?: string;
  minHeight?: number;
}) {
  return (
    <View
      testID={testID}
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: space.xl,
        minHeight: Math.max(layout.touchMin, minHeight),
        paddingVertical: space.sm,
        paddingStart: space.xxl,
        paddingEnd: space.lg,
      }}
    >
      {glyph !== undefined ? (
        <View style={{ minWidth: 22, alignItems: "center" }}>{glyph}</View>
      ) : null}
      <View
        accessible
        accessibilityLabel={accessibilityLabel ?? (sub ? `${name}, ${sub}` : name)}
        style={{ flex: 1, minWidth: 0, gap: 2 }}
      >
        <Text numberOfLines={2} style={{ fontFamily: fonts.medium, fontSize: 16, color: semantic.text }}>
          {name}
        </Text>
        {sub ? <Text style={ty.secondary}>{sub}</Text> : null}
      </View>
      {action}
    </View>
  );
}
