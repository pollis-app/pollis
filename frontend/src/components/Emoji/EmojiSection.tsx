import React, { useEffect, useRef, useState } from "react";
import { EmojiCell } from "./EmojiCell";
import type { PickerEmoji } from "./emojiSearch";
import { pickerEmojiId } from "./emojiSearch";

/** Cells per row. Must match the `grid-cols-*` class below — the picker's
 * arrow-key navigation uses it to move a whole row at a time. */
export const EMOJI_COLUMNS = 9;

interface EmojiSectionProps {
  id: string;
  title: string;
  items: readonly PickerEmoji[];
  toneIndex: number;
  /** Index of this section's FIRST cell in the picker's flat nav order. */
  baseIndex: number;
  onSelect: (item: PickerEmoji) => void;
  onPreview: (item: PickerEmoji | null) => void;
  scrollRoot: HTMLElement | null;
}

/**
 * One titled block of the picker, with a sticky header.
 *
 * The grid is mounted LAZILY. A full standard emoji set is ~1800 cells, and
 * mounting all of them makes opening the picker visibly slow in the WebKitGTK
 * webview — the categories a user never scrolls to should not cost anything. An
 * `IntersectionObserver` with a generous root margin swaps a correctly-sized
 * placeholder for the real grid shortly before the section comes into view, and
 * once mounted the grid stays mounted so scrolling back up never re-flashes.
 *
 * The placeholder is sized from the item count so the scrollbar is honest from
 * the first frame — otherwise the scroll position would jump as sections mount
 * underneath the user.
 */
export const EmojiSection: React.FC<EmojiSectionProps> = ({
  id,
  title,
  items,
  toneIndex,
  baseIndex,
  onSelect,
  onPreview,
  scrollRoot,
}) => {
  const ref = useRef<HTMLDivElement>(null);
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    if (mounted) {
      return;
    }
    const element = ref.current;
    if (!element) {
      return;
    }
    // No IntersectionObserver (very old webview, or a test environment) →
    // render everything rather than render nothing.
    if (typeof IntersectionObserver === "undefined") {
      setMounted(true);
      return;
    }
    const observer = new IntersectionObserver(
      (entries) => {
        if (entries.some((entry) => entry.isIntersecting)) {
          setMounted(true);
        }
      },
      { root: scrollRoot, rootMargin: "400px 0px" },
    );
    observer.observe(element);
    return () => {
      observer.disconnect();
    };
  }, [mounted, scrollRoot]);

  if (items.length === 0) {
    return null;
  }

  // 2rem cell + 0.125rem gap, rounded up to whole rows.
  const rows = Math.ceil(items.length / EMOJI_COLUMNS);
  const placeholderHeight = `${rows * 2.125}rem`;

  return (
    <section ref={ref} data-emoji-section={id} className="mb-1">
      <h3 className="section-label sticky top-0 z-10 py-1 bg-surface-raised">{title}</h3>
      {mounted ? (
        <div className="grid grid-cols-9 gap-0.5" role="group" aria-label={title}>
          {items.map((item, offset) => (
            <EmojiCell
              key={pickerEmojiId(item)}
              item={item}
              toneIndex={toneIndex}
              index={baseIndex + offset}
              onSelect={onSelect}
              onPreview={onPreview}
            />
          ))}
        </div>
      ) : (
        <div style={{ height: placeholderHeight }} aria-hidden="true" />
      )}
    </section>
  );
};
