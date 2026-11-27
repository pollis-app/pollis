import React from "react";
import type { PickerEmoji } from "./emojiSearch";
import { emojiDisplayChar, pickerEmojiId } from "./emojiSearch";
import { emojiDisplayName, type EmojiAnnotationStack } from "./emojiAnnotations";
import { CustomEmojiImage } from "./CustomEmojiImage";
import { PressButton } from "../ui/PressButton";

interface EmojiCellProps {
  item: PickerEmoji;
  toneIndex: number;
  annotations: EmojiAnnotationStack;
  index: number;
  onSelect: (item: PickerEmoji) => void;
  onPreview: (item: PickerEmoji | null) => void;
}

/** A real `<button>` (`PressButton`, acting on click); `data-emoji-index` is its flat nav position. */
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
    <PressButton
      activateOn="click"
      data-testid="emoji-cell"
      data-emoji-index={index}
      data-emoji-id={pickerEmojiId(item)}
      aria-label={label}
      title={label}
      onClick={() => onSelect(item)}
      onMouseEnter={() => onPreview(item)}
      onFocus={() => onPreview(item)}
      onMouseLeave={() => onPreview(null)}
      className="motion-hover-grow flex items-center justify-center w-8 h-8 text-glyph leading-none
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
    </PressButton>
  );
};
