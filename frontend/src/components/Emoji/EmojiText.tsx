import React from "react";
import { splitEmojiSegments } from "./emojiTokens";
import { CustomEmojiImage } from "./CustomEmojiImage";

interface EmojiTextProps {
  text: string;
  /** Renders the text between tokens; defaults to plain text. */
  renderText?: (text: string, key: string) => React.ReactNode;
  /** Emoji-only message, rendered large. */
  jumbo?: boolean;
}

/** Turns well-formed `<:shortcode:hash>` tokens into images. Rendering is ungated (#848): only sending is permissioned. */
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
