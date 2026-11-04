import React from "react";
import { CustomEmojiImage } from "../Emoji/CustomEmojiImage";
import { splitEmojiSegments } from "../Emoji/emojiTokens";

/** Unicode or a custom `<:name:hash>` token (#848). */
export const ReactionEmoji: React.FC<{ emoji: string }> = ({ emoji }) => {
  const segments = splitEmojiSegments(emoji);
  return (
    <>
      {segments.map((segment, index) =>
        segment.kind === "emoji" ? (
          <CustomEmojiImage
            key={index}
            contentHash={segment.contentHash}
            shortcode={segment.shortcode}
            sizeRem={1}
          />
        ) : (
          <span key={index}>{segment.text}</span>
        ),
      )}
    </>
  );
};

/** What a label or tooltip says for a reaction: a custom token as `:name:`. */
export function spokenEmoji(emoji: string): string {
  const segments = splitEmojiSegments(emoji);
  if (segments.length === 1 && segments[0].kind === "emoji") {
    return `:${segments[0].shortcode}:`;
  }
  return emoji;
}
