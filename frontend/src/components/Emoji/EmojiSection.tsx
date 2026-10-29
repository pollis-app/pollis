import React, { useEffect, useRef, useState } from "react";
import { EmojiCell } from "./EmojiCell";
import type { EmojiAnnotationStack } from "./emojiAnnotations";
import type { PickerEmoji } from "./emojiSearch";
import { pickerEmojiId } from "./emojiSearch";

/** Must match the `grid-cols-*` class; arrow keys move by a row. */
export const EMOJI_COLUMNS = 9;

interface EmojiSectionProps {
  id: string;
  title: string;
  items: readonly PickerEmoji[];
  toneIndex: number;
  annotations: EmojiAnnotationStack;
  baseIndex: number;
  onSelect: (item: PickerEmoji) => void;
  onPreview: (item: PickerEmoji | null) => void;
  scrollRoot: HTMLElement | null;
}

/** A titled block whose grid mounts lazily (IntersectionObserver) behind a correctly-sized placeholder; ~1800 cells are slow on WebKitGTK. */
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
