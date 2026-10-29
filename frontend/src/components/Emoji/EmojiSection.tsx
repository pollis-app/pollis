import React, { useEffect, useRef, useState } from "react";
import { EmojiCell } from "./EmojiCell";
import type { EmojiAnnotationStack } from "./emojiAnnotations";
import type { PickerEmoji } from "./emojiSearch";
import { pickerEmojiId } from "./emojiSearch";

/** Cells per row; must match the `grid-cols-*` class below (row-wise arrow nav). */
export const EMOJI_COLUMNS = 9;

interface EmojiSectionProps {
  id: string;
  title: string;
  items: readonly PickerEmoji[];
  toneIndex: number;
  annotations: EmojiAnnotationStack;
  /** Index of this section's FIRST cell in the picker's flat nav order. */
  baseIndex: number;
  onSelect: (item: PickerEmoji) => void;
  onPreview: (item: PickerEmoji | null) => void;
  scrollRoot: HTMLElement | null;
}

/**
 * One titled picker section with a sticky header. The grid mounts lazily (~1800
 * cells is visibly slow in WebKitGTK) via an `IntersectionObserver`, behind a
 * placeholder sized from the item count so the scrollbar is honest, and stays
 * mounted once shown.
 */
export const EmojiSection: React.FC<EmojiSectionProps> = ({
  id,
  title,
  items,
  toneIndex,
  annotations,
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
    // No IntersectionObserver (old webview, tests): render everything.
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
              annotations={annotations}
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
