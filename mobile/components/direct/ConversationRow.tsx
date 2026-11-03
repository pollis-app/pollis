import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Badge } from "../ui";
import { UserAvatar } from "../UserAvatar";
import {
  semantic,
  type as ty,
  fonts,
  r,
  space,
  layout,
} from "../../theme/tokens";

/** One conversation in the Direct list; unread is said three ways, never colour alone. One control. */
export function ConversationRow({
  name,
  avatars,
  preview,
  own,
  time,
  unread = 0,
  selected,
  onPress,
  onPressIn,
  testID,
  unreadTestID,
}: {
  name: string;
  // Two or more draws the overlapped group avatar.
  avatars: ConversationAvatarPerson[];
  preview?: string | null;
  own?: boolean;
  time?: string | null;
  unread?: number;
  selected?: boolean;
  onPress: () => void;
  // Touch down, never an action: a row in a scroller may become a scroll (#1610 prefetch).
  onPressIn?: () => void;
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
      onPressIn={onPressIn}
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
      <ConversationAvatar people={avatars} testID={testID ? `${testID}-avatar` : undefined} />
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

export interface ConversationAvatarPerson {
  label: string;
  userId?: string;
  avatarKey?: string | null;
}

function ConversationAvatar({
  people,
  testID,
}: {
  people: ConversationAvatarPerson[];
  testID?: string;
}) {
  if (people.length < 2) {
    return (
      <UserAvatar
        testID={testID}
        userId={people[0]?.userId}
        avatarKey={people[0]?.avatarKey}
        label={people[0]?.label}
        size={layout.touchMin}
      />
    );
  }
  return (
    <View
      accessible={false}
      importantForAccessibility="no-hide-descendants"
      style={{ width: layout.touchMin, height: layout.touchMin }}
    >
      <UserAvatar
        userId={people[0].userId}
        avatarKey={people[0].avatarKey}
        label={people[0].label}
        size={30}
        style={{ position: "absolute", top: 0, start: 0 }}
      />
      <UserAvatar
        userId={people[1].userId}
        avatarKey={people[1].avatarKey}
        label={people[1].label}
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
