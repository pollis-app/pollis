import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { fonts, semantic, type as ty, r } from "../../theme/tokens";
import type { Reaction } from "../../hooks/queries/useReactions";
import { splitEmojiSegments } from "../emoji/emojiTokens";
import { CustomEmojiImage } from "../emoji/CustomEmojiImage";

/**
 * The emoji face of one pill. A reaction's emoji string is opaque — either a
 * Unicode emoji or a `<:name:hash>` custom token — so both must render.
 */
function ReactionFace({ emoji }: { emoji: string }) {
  const segments = splitEmojiSegments(emoji);
  const first = segments[0];
  if (segments.length === 1 && first.kind === "emoji") {
    return (
      <CustomEmojiImage
        shortcode={first.shortcode}
        contentHash={first.contentHash}
        size={18}
      />
    );
  }
  return <Text style={{ fontSize: 16 }}>{emoji}</Text>;
}

/**
 * Reaction pills under a message. Tapping a pill toggles the current user's
 * reaction for that emoji (remove when already present, add otherwise).
 */
export function ReactionPills({
  messageId,
  reactions,
  currentUserId,
  onToggle,
}: {
  messageId: string;
  reactions: Reaction[];
  currentUserId?: string;
  onToggle: (emoji: string, reacted: boolean) => void;
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
            onPress={() => onToggle(reaction.emoji, reacted)}
            // 32pt visual pill; the slop brings the target to 44pt.
            hitSlop={6}
            style={({ pressed }) => ({
              flexDirection: "row",
              alignItems: "center",
              gap: 6,
              minHeight: 32,
              paddingHorizontal: 10,
              borderRadius: r.pill,
              // Reacted = accent outline, no fill: an accent fill sat behind
              // the emoji and swallowed the default (yellow) skin tone. Every
              // pill carries the border so toggling never shifts the layout.
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
            {/* Reacted = accent outline + bold count (the others: dark fill +
                medium count), never colour alone. */}
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
