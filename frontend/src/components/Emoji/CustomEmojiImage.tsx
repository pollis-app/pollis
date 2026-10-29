import React from "react";
import { useEmojiImage } from "./useEmojiImage";

interface CustomEmojiImageProps {
  contentHash: string;
  shortcode: string;
  /** Rendered edge length. `rem` so it tracks the user's font setting. */
  sizeRem?: number;
  className?: string;
}

/**
 * One custom emoji, rendered inline. Falls back to the literal `:shortcode:`
 * when the image can't be resolved (deleted, offline), which is why the
 * shortcode travels with the hash in the wire token.
 *
 * The shortcode is only ever a text child or `alt`/`title`, never markup.
 */
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
    // A sized placeholder so text doesn't reflow when the image lands.
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
