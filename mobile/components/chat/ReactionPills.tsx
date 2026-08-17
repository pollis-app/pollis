import { View, Text, Pressable } from "react-native";
import { semantic, type as ty, r, t } from "../../theme/tokens";
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
        size={16}
      />
    );
  }
  return <Text style={{ fontSize: 14 }}>{emoji}</Text>;
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
  if (reactions.length === 0) {
    return null;
  }
  return (
    <View
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        gap: 6,
        marginTop: 6,
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
            accessibilityLabel={`${reaction.emoji} ${reaction.count}${reacted ? ", you reacted" : ""}`}
            onPress={() => onToggle(reaction.emoji, reacted)}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: 5,
              paddingHorizontal: 8,
              paddingVertical: 3,
              borderWidth: 1,
              borderColor: reacted ? semantic.accent : semantic.hair,
              borderRadius: r.lg,
              backgroundColor: reacted ? t(0.12) : "transparent",
            }}
          >
            <ReactionFace emoji={reaction.emoji} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: reacted ? semantic.accent : semantic.ink2,
              }}
            >
              {reaction.count}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}
