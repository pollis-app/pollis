import React from "react";
import { splitEmojiSegments } from "./emojiTokens";
import { CustomEmojiImage } from "./CustomEmojiImage";

interface EmojiTextProps {
  text: string;
  /**
   * How to render the ordinary text between tokens. Defaults to plain text.
   * `MessageBody` passes its linkifier here so links keep working inside a
   * message that also carries custom emoji.
   */
  renderText?: (text: string, key: string) => React.ReactNode;
  /**
   * True when the message is nothing but emoji, which Discord renders large.
   * Purely cosmetic.
   */
  jumbo?: boolean;
}

/**
 * Render message text with `<:shortcode:hash>` custom-emoji tokens turned into
 * images.
 *
 * Rendering asks no permission question at all — that is the #848 rule working
 * as specified. A user in none of the groups that registered an emoji cannot
 * *send* it, but must see it when someone who can does, which is exactly why
 * the objects are stored unencrypted and their fetch is ungated.
 *
 * Anything that is not a well-formed token stays literal text (see
 * `splitEmojiSegments`), so this can be applied to every message body without
 * risk of eating something that merely looks like a token.
 */
export const EmojiText: React.FC<EmojiTextProps> = ({ text, renderText, jumbo = false }) => {
  const segments = splitEmojiSegments(text);

  return (
    <>
      {segments.map((segment, index) => {
        const key = `${segment.kind}-${index}`;
        if (segment.kind === "text") {
          if (renderText) {
            return <React.Fragment key={key}>{renderText(segment.text, key)}</React.Fragment>;
          }
          return <React.Fragment key={key}>{segment.text}</React.Fragment>;
        }
        return (
          <CustomEmojiImage
            key={key}
            contentHash={segment.contentHash}
            shortcode={segment.shortcode}
            sizeRem={jumbo ? 3 : 1.375}
          />
        );
      })}
    </>
  );
};
