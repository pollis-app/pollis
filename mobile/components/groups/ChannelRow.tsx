import { Pressable, Text, View } from "react-native";
import { semantic, fonts, r, space, layout } from "../../theme/tokens";
import { Badge, Dot } from "../ui";

// One row of the group panel's channel list (Main.dc.html): 44pt, a small
// 16pt glyph, 16pt name. Selected = accentSoft fill + accent text; unread =
// bold text + trailing dot (or a count badge when `count` is given); read =
// dim. The row is ONE accessible element — callers pass the full spoken
// label (name + selection / unread state).
export function ChannelRow({
  icon,
  name,
  selected,
  unread,
  count,
  value,
  onPress,
  testID,
  accessibilityLabel,
  dimWhenRead = true,
}: {
  // Rendered at 16pt by the caller (e.g. <Icon.hash size={16} />).
  icon: React.ReactNode;
  name: string;
  selected?: boolean;
  unread?: boolean;
  // A trailing count badge (mentions / waiting requests); replaces the dot.
  count?: number;
  // A trailing muted value (e.g. the member count).
  value?: string;
  onPress: () => void;
  testID?: string;
  accessibilityLabel: string;
  // Channel rows dim when read; "This group" rows always dim.
  dimWhenRead?: boolean;
}) {
  const color = selected
    ? semantic.accent
    : unread || !dimWhenRead
      ? semantic.text
      : semantic.dim;
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessible
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ selected: !!selected }}
      style={({ pressed }) => ({
        minHeight: layout.touchMin,
        borderRadius: r.sm,
        flexDirection: "row",
        alignItems: "center",
        gap: space.md,
        paddingHorizontal: space.md,
        paddingVertical: 4,
        backgroundColor: selected
          ? semantic.accentSoft
          : pressed
            ? semantic.raised
            : "transparent",
      })}
    >
      <View accessible={false}>{icon}</View>
      <Text
        numberOfLines={1}
        style={{
          flex: 1,
          fontFamily: selected ? fonts.semibold : unread ? fonts.bold : fonts.medium,
          fontSize: 16,
          color,
        }}
      >
        {name}
      </Text>
      {value !== undefined ? (
        <Text style={{ fontFamily: fonts.regular, fontSize: 14, color: semantic.muted }}>
          {value}
        </Text>
      ) : null}
      {count !== undefined && count > 0 ? (
        <Badge>{count > 99 ? "99+" : count}</Badge>
      ) : unread ? (
        <View style={{ marginEnd: 7 }}>
          <Dot />
        </View>
      ) : null}
    </Pressable>
  );
}
