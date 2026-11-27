import { Pressable, View } from "react-native";
import { Text } from "../Text";
import { semantic, fonts, r, space, layout, typeSize } from "../../theme/tokens";
import { Badge, Dot } from "../ui";
import { useLateArrival } from "../../hooks/useLateArrival";
import { appStore } from "../../stores/appStore";
import { Icon } from "../icons";

// One row of the group panel's channel list; ONE accessible element, so callers pass the full spoken label.
export function ChannelRow({
  icon,
  name,
  selected,
  unread,
  count,
  value,
  onPress,
  onPressIn,
  testID,
  accessibilityLabel,
  dimWhenRead = true,
  muted = false,
}: {
  icon: React.ReactNode;
  name: string;
  selected?: boolean;
  unread?: boolean;
  count?: number;
  value?: string;
  onPress: () => void;
  // Touch down, never an action: a row in a scroller may become a scroll (#1610 prefetch).
  onPressIn?: () => void;
  testID?: string;
  accessibilityLabel: string;
  dimWhenRead?: boolean;
  // A muted channel (#1431): quieter, never bold, with a bell-off glyph; the caller says "muted".
  muted?: boolean;
}) {
  const badgePops = useLateArrival(count !== undefined && count > 0, appStore.unreadHydrated);
  const color = selected
    ? semantic.accent
    : muted
      ? semantic.muted
      : unread || !dimWhenRead
        ? semantic.text
        : semantic.dim;
  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      onPressIn={onPressIn}
      accessible
      accessibilityRole="button"
      accessibilityLabel={accessibilityLabel}
      accessibilityState={{ selected: !!selected }}
      style={({ pressed }) => ({
        minHeight: layout.touchMin,
        borderRadius: r.box,
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
          fontFamily: selected ? fonts.semibold : unread && !muted ? fonts.bold : fonts.medium,
          fontSize: typeSize.rowTitle,
          color,
        }}
      >
        {name}
      </Text>
      {muted ? (
        <View accessible={false}>
          <Icon.bellOff size={16} color={selected ? semantic.accent : semantic.muted} />
        </View>
      ) : null}
      {value !== undefined ? (
        <Text style={{ fontFamily: fonts.regular, fontSize: typeSize.secondary, color: semantic.muted }}>
          {value}
        </Text>
      ) : null}
      {count !== undefined && count > 0 ? (
        <Badge pop={badgePops}>{count > 99 ? "99+" : count}</Badge>
      ) : unread ? (
        <View style={{ marginEnd: 7 }}>
          <Dot />
        </View>
      ) : null}
    </Pressable>
  );
}
