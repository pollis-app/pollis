import React from "react";
import { splitEmojiSegments } from "./emojiTokens";
import { CustomEmojiImage } from "./CustomEmojiImage";

interface EmojiTextProps {
  text: string;
  /** Renders text between tokens; `MessageBody` passes its linkifier. */
  renderText?: (text: string, key: string) => React.ReactNode;
  /** Message is nothing but emoji (rendered large). */
  jumbo?: boolean;
}

/**
 * Render message text with `<:shortcode:hash>` tokens as images. No permission
 * check: per #848 anyone must see an emoji someone could send. Malformed tokens
 * stay literal text.
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
