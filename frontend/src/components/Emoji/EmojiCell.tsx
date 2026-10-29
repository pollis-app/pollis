import React from "react";
import type { PickerEmoji } from "./emojiSearch";
import { emojiDisplayChar, pickerEmojiId } from "./emojiSearch";
import { emojiDisplayName, type EmojiAnnotationStack } from "./emojiAnnotations";
import { CustomEmojiImage } from "./CustomEmojiImage";

interface EmojiCellProps {
  item: PickerEmoji;
  toneIndex: number;
  annotations: EmojiAnnotationStack;
  index: number;
  onSelect: (item: PickerEmoji) => void;
  onPreview: (item: PickerEmoji | null) => void;
}

/** A real `<button>`; `data-emoji-index` is its flat nav position. */
export const EmojiCell: React.FC<EmojiCellProps> = ({
  item,
  toneIndex,
  annotations,
  index,
  onSelect,
  onPreview,
}) => {
  const label =
    item.kind === "standard"
      ? emojiDisplayName(item.emoji, annotations)
      : `:${item.emoji.shortcode}:`;

  return (
    <button
      type="button"
      data-testid="emoji-cell"
      data-emoji-index={index}
      data-emoji-id={pickerEmojiId(item)}
      aria-label={label}
      title={label}
      onClick={() => onSelect(item)}
      onMouseEnter={() => onPreview(item)}
      onFocus={() => onPreview(item)}
      onMouseLeave={() => onPreview(null)}
      className="flex items-center justify-center w-8 h-8 text-xl leading-none
 hover:bg-hover focus:bg-active
                 focus:outline-none rounded-chip"
    >
      {item.kind === "standard" ? (
        <span>{emojiDisplayChar(item.emoji, toneIndex)}</span>
      ) : (
        <CustomEmojiImage
          contentHash={item.emoji.content_hash}
          shortcode={item.emoji.shortcode}
          sizeRem={1.5}
        />
      )}
    </button>
  );
};
