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
 * One custom emoji, rendered inline.
 *
 * Falls back to the literal `:shortcode:` text when the image cannot be
 * resolved — the emoji may have been deleted, or the device may be offline.
 * That fallback is the whole reason the shortcode travels alongside the hash in
 * the wire token: a message must stay readable when its decorations do not
 * load.
 *
 * The shortcode is only ever set as a text child and an `alt`/`title` value —
 * never interpolated into markup — and its alphabet (`[a-z0-9_]`) cannot carry
 * any anyway. Both, because one of them being true is not a reason to leave the
 * other to chance.
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
    // A sized placeholder rather than nothing, so text does not reflow when the
    // image lands.
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
