import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar, Badge } from "../ui";
import {
  semantic,
  type as ty,
  fonts,
  r,
  space,
  layout,
} from "../../theme/tokens";

/**
 * One conversation in the Direct list (Messages.dc.html): 64pt row, 44pt
 * avatar (two overlapped 30pt avatars for a group DM), name over a one-line
 * preview, time + unread count at the end. Unread is said three ways — bold
 * name and preview, accent time, a count badge — never by colour alone. The
 * whole row is one control with one spoken label.
 */
export function ConversationRow({
  name,
  avatarLabels,
  preview,
  own,
  time,
  unread = 0,
  selected,
  onPress,
  testID,
  unreadTestID,
}: {
  name: string;
  // One label for a 1:1 DM; two or more draws the overlapped group avatar.
  avatarLabels: string[];
  preview?: string | null;
  // The last message is the viewer's own: prefixed "You:".
  own?: boolean;
  time?: string | null;
  unread?: number;
  selected?: boolean;
  onPress: () => void;
  testID?: string;
  unreadTestID?: string;
}) {
  const { t } = useTranslation("mobile");
  const isUnread = unread > 0;
  const shownPreview = preview
    ? own
      ? t("direct.ownPreview", { text: preview })
      : preview
    : null;
  const spoken = [
    isUnread ? t("tabs.withUnread", { label: name, count: unread }) : name,
    shownPreview,
    time,
  ]
    .filter(Boolean)
    .join(", ");

  return (
    <Pressable
      testID={testID}
      onPress={onPress}
      accessible
      accessibilityRole="button"
      accessibilityLabel={spoken}
      accessibilityState={{ selected: !!selected }}
      style={({ pressed }) => ({
        flexDirection: "row",
        alignItems: "center",
        gap: space.lg,
        minHeight: 64,
        paddingVertical: space.sm,
        paddingHorizontal: space.sm,
        borderRadius: r.md,
        backgroundColor: selected
          ? semantic.accentSoft
          : pressed
            ? semantic.raised
            : "transparent",
      })}
    >
      <ConversationAvatar labels={avatarLabels} />
      <View style={{ flex: 1, minWidth: 0, gap: 2 }}>
        <View
          style={{
            flexDirection: "row",
            alignItems: "baseline",
            gap: space.sm,
          }}
        >
          <Text
            numberOfLines={1}
            style={{
              flex: 1,
              fontFamily: isUnread ? fonts.bold : fonts.semibold,
              fontSize: 16,
              color: selected ? semantic.accent : semantic.text,
            }}
          >
            {name}
          </Text>
          {time ? (
            <Text
              style={[
                ty.meta,
                isUnread
                  ? { color: semantic.accent, fontFamily: fonts.semibold }
                  : null,
              ]}
            >
              {time}
            </Text>
          ) : null}
        </View>
        {shownPreview || isUnread ? (
          <View
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: space.sm,
            }}
          >
            <Text
              numberOfLines={1}
              style={[
                ty.secondary,
                { flex: 1, minWidth: 0 },
                isUnread
                  ? { color: semantic.text, fontFamily: fonts.semibold }
                  : null,
              ]}
            >
              {shownPreview ?? ""}
            </Text>
            {isUnread ? (
              <Badge testID={unreadTestID}>
                {unread > 99 ? "99+" : unread}
              </Badge>
            ) : null}
          </View>
        ) : null}
      </View>
    </Pressable>
  );
}

// 44pt circle with an initial, or two overlapped 30pt circles for a group.
function ConversationAvatar({ labels }: { labels: string[] }) {
  if (labels.length < 2) {
    return <Avatar label={labels[0]} size={layout.touchMin} />;
  }
  return (
    <View
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={{ width: layout.touchMin, height: layout.touchMin }}
    >
      <Avatar
        label={labels[0]}
        size={30}
        style={{ position: "absolute", top: 0, start: 0 }}
      />
      <Avatar
        label={labels[1]}
        size={30}
        style={{
          position: "absolute",
          bottom: 0,
          end: 0,
          borderWidth: 2,
          borderColor: semantic.bg,
        }}
      />
    </View>
  );
}
