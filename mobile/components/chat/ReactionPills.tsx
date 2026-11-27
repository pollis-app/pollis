import { useEffect, useRef } from "react";
import { Pressable, View } from "react-native";
import Animated, {
  ReduceMotion,
  useAnimatedStyle,
  useSharedValue,
  withSequence,
  withTiming,
} from "react-native-reanimated";
import { Text } from "../Text";
import { useTranslation } from "react-i18next";
import { fonts, semantic, type as ty, r, typeSize } from "../../theme/tokens";
import type { Reaction } from "../../hooks/queries/useReactions";
import { splitEmojiSegments } from "../emoji/emojiTokens";
import { CustomEmojiImage } from "../emoji/CustomEmojiImage";
import { haptic } from "../../lib/haptics";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import {
  EASE_OUT,
  LATE_FADE_IN,
  MOTION,
  POP_IN,
  REDUCED_FADE_IN,
  ROLL_IN,
} from "../../theme/motion";
import { useReduceMotion } from "../../hooks/useReduceMotion";
import { pillMotion, wasJustAdded } from "../../lib/reactionMotion";
import type { TFunction } from "i18next";

/** A reaction's emoji is a Unicode emoji or a `<:name:hash>` custom token. */
export function ReactionFace({ emoji, size = 18 }: { emoji: string; size?: number }) {
  const segments = splitEmojiSegments(emoji);
  const first = segments[0];
  if (segments.length === 1 && first.kind === "emoji") {
    return (
      <CustomEmojiImage
        shortcode={first.shortcode}
        contentHash={first.contentHash}
        size={size}
        fadeIn
      />
    );
  }
  return <Text style={{ fontSize: size - 2 }}>{emoji}</Text>;
}

export function ReactionPills({
  messageId,
  reactions,
  fadeIn = false,
  currentUserId,
  onToggle,
  onShowWho,
}: {
  messageId: string;
  reactions: Reaction[];
  // The row painted without pills: fade them in within the space they take.
  fadeIn?: boolean;
  currentUserId?: string;
  onToggle: (emoji: string, reacted: boolean) => void;
  // Long press: who reacted (the breakdown sheet). The pill is the responder, so the row's own long press never fires.
  onShowWho?: (emoji: string) => void;
}) {
  const { t } = useTranslation("chat");
  const reduceMotion = useReduceMotion();
  // What this row last showed (#2125): null on its first render, so a fresh paint never moves.
  const shown = useRef<Map<string, number> | null>(null);
  const motion = pillMotion(shown.current, reactions, (emoji) => wasJustAdded(messageId, emoji));
  useEffect(() => {
    shown.current = new Map(reactions.map((r) => [r.emoji, r.count]));
  }, [reactions]);
  if (reactions.length === 0) {
    return null;
  }
  return (
    <Animated.View
      // An animated view is never flattened: without an id, iOS hides the pills inside an accessible row (#1895).
      testID={`reactions-${messageId}`}
      entering={fadeIn ? LATE_FADE_IN : undefined}
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        gap: 6,
        marginTop: 8,
      }}
    >
      {reactions.map((reaction) => (
        <ReactionPill
          key={reaction.emoji}
          messageId={messageId}
          reaction={reaction}
          reacted={currentUserId ? reaction.user_ids.includes(currentUserId) : false}
          pop={motion.pop.has(reaction.emoji)}
          roll={motion.roll.has(reaction.emoji)}
          reduceMotion={reduceMotion}
          t={t}
          onToggle={onToggle}
          onShowWho={onShowWho}
        />
      ))}
    </Animated.View>
  );
}

const BUMP_UP = { duration: MOTION.fastMs / 2, easing: EASE_OUT, reduceMotion: ReduceMotion.Never };
const BUMP_DOWN = { duration: MOTION.fastMs, easing: EASE_OUT, reduceMotion: ReduceMotion.Never };

/** One pill: pops in when new, rolls its count when it changes, bumps when tapped (#2125). */
function ReactionPill({
  messageId,
  reaction,
  reacted,
  pop,
  roll,
  reduceMotion,
  t,
  onToggle,
  onShowWho,
}: {
  messageId: string;
  reaction: Reaction;
  reacted: boolean;
  pop: boolean;
  roll: boolean;
  reduceMotion: boolean;
  t: TFunction;
  onToggle: (emoji: string, reacted: boolean) => void;
  onShowWho?: (emoji: string) => void;
}) {
  const scale = useSharedValue(1);
  const bumpStyle = useAnimatedStyle(() => ({ transform: [{ scale: scale.value }] }));
  const onPress = () => {
    // Reduce Motion: no bump; the count and the outline still say what changed.
    if (!reduceMotion) {
      scale.value = withSequence(
        withTiming(MOTION.bumpScale, BUMP_UP),
        withTiming(1, BUMP_DOWN),
      );
    }
    onToggle(reaction.emoji, reacted);
  };
  return (
    <Animated.View
      testID={`pill-wrap-${messageId}-${reaction.emoji}`}
      entering={pop ? (reduceMotion ? REDUCED_FADE_IN : POP_IN) : undefined}
      style={bumpStyle}
    >
      <Pressable
        testID={`pill-reaction-${messageId}-${reaction.emoji}`}
        accessibilityRole="button"
        accessibilityState={{ selected: reacted }}
        accessibilityLabel={
          reacted
            ? t("mobile:chat.reactionPillReacted", {
                emoji: reaction.emoji,
                count: reaction.count,
              })
            : t("reactions.pillLabel", {
                emoji: reaction.emoji,
                count: reaction.count,
              })
        }
        accessibilityHint={onShowWho ? t("mobile:reactions.pillHint") : undefined}
        accessibilityActions={
          onShowWho ? [{ name: "longpress", label: t("reactions.viewWho") }] : undefined
        }
        onAccessibilityAction={(e) => {
          if (e.nativeEvent.actionName === "longpress") {
            onShowWho?.(reaction.emoji);
          }
        }}
        onPress={onPress}
        onLongPress={
          onShowWho
            ? () => {
                // Every long press confirms itself by touch (#1286).
                haptic("longPress");
                onShowWho(reaction.emoji);
              }
            : undefined
        }
        delayLongPress={LONG_PRESS_MS}
        hitSlop={6}
        style={({ pressed }) => ({
          flexDirection: "row",
          alignItems: "center",
          gap: 6,
          minHeight: 32,
          paddingHorizontal: 10,
          borderRadius: r.pill,
          // Accent outline, no fill: a fill swallowed the default skin tone. Every pill has the border.
          borderWidth: 1.5,
          borderColor: reacted ? semantic.accent : "transparent",
          backgroundColor: pressed
            ? semantic.high
            : reacted
              ? "transparent"
              : semantic.raised,
        })}
      >
        <ReactionFace emoji={reaction.emoji} />
        {/* The count rolls up inside its own line when it changes. */}
        <View style={{ overflow: "hidden" }}>
          <Animated.View
            key={reaction.count}
            entering={roll ? (reduceMotion ? REDUCED_FADE_IN : ROLL_IN) : undefined}
          >
            {/* Reacted = accent outline + bold count, never colour alone. */}
            <Text
              style={[
                ty.meta,
                {
                  fontFamily: reacted ? fonts.bold : fonts.medium,
                  fontSize: typeSize.section,
                  color: semantic.accent,
                },
              ]}
            >
              {reaction.count}
            </Text>
          </Animated.View>
        </View>
      </Pressable>
    </Animated.View>
  );
}
