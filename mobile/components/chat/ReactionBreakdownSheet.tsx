import { useEffect, useMemo, useState } from "react";
import { View, Text, Pressable, ScrollView, FlatList, useWindowDimensions } from "react-native";
import { useTranslation } from "react-i18next";
import Animated, { useAnimatedStyle, useSharedValue, withTiming } from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { Gesture, GestureDetector, GestureHandlerRootView } from "react-native-gesture-handler";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { ListRow } from "../ui";
import { UserAvatar } from "../UserAvatar";
import { useTheme } from "../theme";
import { fonts, r, semantic, space, type as ty } from "../../theme/tokens";
import { SheetOverlay, afterSheetClose } from "./SheetOverlay";
import { ReactionFace } from "./ReactionPills";
import { splitEmojiSegments } from "../emoji/emojiTokens";
import { useUserProfiles } from "../../hooks/queries/useUserProfile";
import type { Reaction } from "../../hooks/queries/useReactions";
import {
  buildReactionBreakdown,
  mergeProfiles,
  reactorIds,
  selectTab,
  type Reactor,
  type ReactorProfile,
  type ReactionTab,
} from "../../lib/reactionBreakdown";

// The sheet's chrome above the body (title row, paddings, gap), so the whole card sits at half height.
const CHROME = 96;
// Drag distance that expands, collapses or closes.
const DRAG_STEP = 48;

/** What a screen reader says for a reaction: a custom `<:name:hash>` token as `:name:`. */
export function spokenEmoji(emoji: string): string {
  const segments = splitEmojiSegments(emoji);
  if (segments.length === 1 && segments[0].kind === "emoji") {
    return `:${segments[0].shortcode}:`;
  }
  return emoji;
}

/**
 * Who reacted with what (Discord's long-press reaction sheet): a tab per emoji
 * (All first when there are several), the long-pressed one selected, and the
 * people under it in reaction order. Half height; drag the tab strip up to
 * expand, down to collapse or close.
 */
export function ReactionBreakdownSheet({
  messageId,
  reactions,
  initialEmoji,
  selfId,
  roster,
  onOpenProfile,
  onClose,
}: {
  messageId: string;
  reactions: readonly Reaction[];
  initialEmoji: string;
  selfId?: string | null;
  // Names and avatar keys the screen already holds (members, self).
  roster: ReadonlyMap<string, ReactorProfile>;
  onOpenProfile?: (userId: string) => void;
  onClose: () => void;
}) {
  useTheme();
  const { t } = useTranslation("chat");
  const { height: windowHeight } = useWindowDimensions();
  const insets = useSafeAreaInsets();
  const ids = useMemo(() => reactorIds(reactions), [reactions]);
  // ONE batched read for everyone on the message; the roster draws the first frame.
  const fetched = useUserProfiles(ids);
  const profiles = useMemo(
    () => mergeProfiles(roster, fetched.data as ReadonlyMap<string, ReactorProfile> | undefined),
    [roster, fetched.data],
  );
  const tabs = useMemo(
    () => buildReactionBreakdown(reactions, selfId, profiles),
    [reactions, selfId, profiles],
  );
  const [wanted, setWanted] = useState<string>(initialEmoji);
  const tab = selectTab(tabs, wanted);

  // Every reaction was taken back while open: nothing left to show.
  const empty = tabs.length === 0;
  useEffect(() => {
    if (empty) {
      onClose();
    }
  }, [empty, onClose]);

  const collapsed = Math.max(240, Math.round(windowHeight * 0.5) - CHROME);
  const expanded = Math.max(collapsed, Math.round(windowHeight * 0.92) - CHROME - insets.top);
  const height = useSharedValue(collapsed);
  const isExpanded = useSharedValue(false);
  const bodyStyle = useAnimatedStyle(() => ({ height: height.value }));

  const drag = useMemo(
    () =>
      Gesture.Pan()
        .activeOffsetY([-12, 12])
        .failOffsetX([-12, 12])
        .onUpdate((e) => {
          const base = isExpanded.value ? expanded : collapsed;
          height.value = Math.min(expanded, Math.max(collapsed * 0.6, base - e.translationY));
        })
        .onEnd((e) => {
          if (e.translationY < -DRAG_STEP) {
            isExpanded.value = true;
          } else if (e.translationY > DRAG_STEP) {
            if (!isExpanded.value) {
              scheduleOnRN(onClose);
              return;
            }
            isExpanded.value = false;
          }
          height.value = withTiming(isExpanded.value ? expanded : collapsed, { duration: 180 });
        }),
    [collapsed, expanded, height, isExpanded, onClose],
  );

  const openProfile = onOpenProfile
    ? (userId: string) => {
        onClose();
        afterSheetClose(() => onOpenProfile(userId));
      }
    : undefined;

  return (
    <SheetOverlay
      title={t("reactions.sheetTitle")}
      onClose={onClose}
      testID="sheet-reaction-breakdown"
      closeTestID="btn-reaction-breakdown-close"
    >
      {/* The explicit height lives outside: GestureHandlerRootView is flex: 1, which collapses in a content-sized card. */}
      <Animated.View style={bodyStyle}>
        <GestureHandlerRootView style={{ flex: 1, gap: space.lg }}>
          <GestureDetector gesture={drag}>
            <View>
              <TabStrip tabs={tabs} selected={tab?.key ?? null} onSelect={setWanted} messageId={messageId} />
            </View>
          </GestureDetector>
          {tab ? (
            <View
              style={{
                flex: 1,
                borderRadius: r.lg,
                overflow: "hidden",
                backgroundColor: semantic.high,
              }}
            >
              <FlatList
                testID="list-reactors"
                data={tab.people}
                keyExtractor={(p) => p.userId}
                extraData={`${tab.key}|${fetched.isPending}`}
                ItemSeparatorComponent={Separator}
                renderItem={({ item }) => (
                  <ReactorRow
                    person={item}
                    tabEmoji={tab.emoji}
                    resolving={fetched.isPending}
                    onPress={openProfile}
                  />
                )}
              />
            </View>
          ) : null}
        </GestureHandlerRootView>
      </Animated.View>
    </SheetOverlay>
  );
}

function Separator() {
  return <View style={{ height: 1, backgroundColor: semantic.hairSoft }} />;
}

function TabStrip({
  tabs,
  selected,
  onSelect,
  messageId,
}: {
  tabs: readonly ReactionTab[];
  selected: string | null;
  onSelect: (key: string) => void;
  messageId: string;
}) {
  const { t } = useTranslation("chat");
  return (
    <ScrollView
      horizontal
      showsHorizontalScrollIndicator={false}
      accessibilityRole="tablist"
      contentContainerStyle={{ gap: space.sm }}
    >
      {tabs.map((tab) => {
        const on = tab.key === selected;
        const label =
          tab.emoji === null
            ? t("reactions.allTabLabel", { count: tab.count })
            : t("reactions.tabLabel", { emoji: spokenEmoji(tab.emoji), count: tab.count });
        return (
          <Pressable
            key={tab.key}
            testID={`tab-reaction-${tab.emoji === null ? "all" : tab.emoji}`}
            accessibilityRole="tab"
            accessibilityState={{ selected: on }}
            accessibilityLabel={label}
            onPress={() => onSelect(tab.key)}
            style={{ minHeight: 44, justifyContent: "center" }}
          >
            {({ pressed }) => (
              <View
                style={{
                  flexDirection: "row",
                  alignItems: "center",
                  gap: space.xs,
                  minHeight: 36,
                  paddingHorizontal: space.lg,
                  borderRadius: r.pill,
                  // Filled, never outlined; the selected tab is the accent tint + bold count.
                  backgroundColor: on ? semantic.accentMid : pressed ? semantic.high : semantic.panel,
                }}
              >
                {tab.emoji === null ? (
                  <Text style={[ty.secondary, { fontFamily: fonts.semibold, color: semantic.text }]}>
                    {t("reactions.all")}
                  </Text>
                ) : (
                  <ReactionFace emoji={tab.emoji} size={20} />
                )}
                <Text
                  testID={`text-reaction-count-${messageId}-${tab.emoji === null ? "all" : tab.emoji}`}
                  style={[
                    ty.secondary,
                    {
                      fontFamily: on ? fonts.bold : fonts.medium,
                      color: on ? semantic.text : semantic.dim,
                    },
                  ]}
                >
                  {tab.count}
                </Text>
              </View>
            )}
          </Pressable>
        );
      })}
    </ScrollView>
  );
}

function ReactorRow({
  person,
  tabEmoji,
  resolving,
  onPress,
}: {
  person: Reactor;
  // Null on the All tab, which also draws each person's emojis.
  tabEmoji: string | null;
  resolving: boolean;
  onPress?: (userId: string) => void;
}) {
  const { t } = useTranslation("chat");
  // Never the raw id: an unnamed person is "Unknown user" (or a placeholder while the read is out).
  const name = person.name ?? (resolving ? "…" : t("reactions.unknownUser"));
  const handle = person.handle ? `@${person.handle}` : null;
  const sub = person.isSelf
    ? handle
      ? `${handle} · ${t("reactions.you")}`
      : t("reactions.you")
    : handle ?? undefined;
  const spokenName = person.isSelf ? t("reactions.selfName", { name }) : name;
  const pressable = !!onPress && !person.isSelf && !!person.name;
  const showEmojis = tabEmoji === null;
  const spokenEmojis = (showEmojis ? person.emojis : [tabEmoji]).map(spokenEmoji).join(" ");
  return (
    <ListRow
      testID={`row-reactor-${person.userId}`}
      glyph={
        <UserAvatar
          userId={person.name ? person.userId : null}
          avatarKey={person.avatarKey}
          label={person.name ?? undefined}
          size={36}
          variant={person.isSelf ? "self" : "default"}
          // The list sits on `high`, the letter avatar's own fill: one step down keeps it visible.
          style={person.isSelf ? undefined : { backgroundColor: semantic.raised }}
        />
      }
      name={name}
      sub={sub}
      end={
        showEmojis ? (
          <View style={{ flexDirection: "row", gap: 4 }}>
            {person.emojis.map((emoji) => (
              <ReactionFace key={emoji} emoji={emoji} size={18} />
            ))}
          </View>
        ) : undefined
      }
      chevron={pressable}
      accessibilityLabel={t("reactions.rowLabel", {
        name: handle && !person.isSelf ? `${spokenName}, ${handle}` : spokenName,
        emojis: spokenEmojis,
      })}
      onPress={pressable ? () => onPress?.(person.userId) : undefined}
    />
  );
}
