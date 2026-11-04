import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { fonts, semantic, type as ty, r } from "../../theme/tokens";
import type { Reaction } from "../../hooks/queries/useReactions";
import { splitEmojiSegments } from "../emoji/emojiTokens";
import { CustomEmojiImage } from "../emoji/CustomEmojiImage";
import { haptic } from "../../lib/haptics";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";

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
      />
    );
  }
  return <Text style={{ fontSize: size - 2 }}>{emoji}</Text>;
}

export function ReactionPills({
  messageId,
  reactions,
  currentUserId,
  onToggle,
  onShowWho,
}: {
  messageId: string;
  reactions: Reaction[];
  currentUserId?: string;
  onToggle: (emoji: string, reacted: boolean) => void;
  // Long press: who reacted (the breakdown sheet). The pill is the responder, so the row's own long press never fires.
  onShowWho?: (emoji: string) => void;
}) {
  const { t } = useTranslation("chat");
  if (reactions.length === 0) {
    return null;
  }
  return (
    <View
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        gap: 6,
        marginTop: 8,
      }}
    >
      {reactions.map((reaction) => {
        const reacted = currentUserId
          ? reaction.user_ids.includes(currentUserId)
          : false;
        return (
          <Pressable
            key={reaction.emoji}
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
            onPress={() => onToggle(reaction.emoji, reacted)}
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
            {/* Reacted = accent outline + bold count, never colour alone. */}
            <Text
              style={[
                ty.meta,
                {
                  fontFamily: reacted ? fonts.bold : fonts.medium,
                  fontSize: 13,
                  color: semantic.accent,
                },
              ]}
            >
              {reaction.count}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
