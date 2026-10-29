import React from "react";
import { useEmojiImage } from "./useEmojiImage";

interface CustomEmojiImageProps {
  contentHash: string;
  shortcode: string;
  sizeRem?: number;
  className?: string;
}

/** One custom emoji; falls back to `:shortcode:` text when unresolvable. The shortcode is only ever text/`alt`, never markup. */
export const CustomEmojiImage: React.FC<CustomEmojiImageProps> = ({
  contentHash,
  shortcode,
  sizeRem = 1.375,
  className = "",
}) => {
  const { url, failed } = useEmojiImage(contentHash);

  if (failed) {
    return (
      <span className="font-mono text-muted" data-testid="custom-emoji-fallback">
        :{shortcode}:
      </span>
    );
  }

  if (!url) {
    // Sized placeholder so text doesn't reflow.
    return (
      <span
        className={`inline-block align-[-0.25em] bg-surface-high ${className}`}
        style={{ width: `${sizeRem}rem`, height: `${sizeRem}rem` }}
        aria-hidden="true"
      />
    );
  }

  return (
    <img
      src={url}
      alt={`:${shortcode}:`}
      title={`:${shortcode}:`}
      draggable={false}
      data-testid="custom-emoji"
      data-shortcode={shortcode}
      className={`inline-block align-[-0.25em] object-contain ${className}`}
      style={{ width: `${sizeRem}rem`, height: `${sizeRem}rem` }}
    />
  );
};
