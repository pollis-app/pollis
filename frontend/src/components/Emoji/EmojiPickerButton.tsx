import React, { Suspense, lazy, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Smile } from "lucide-react";

// The picker carries the full standard emoji table (`emojiData.ts`), which is
// the largest single application module in the bundle, plus the search index
// built on top of it. The panel is already mounted on demand, so none of that
// belongs in the startup chunk (#874). The trigger button — the only part
// always on screen — stays eager.
const EmojiPicker = lazy(() =>
  import("./EmojiPicker").then((m) => ({ default: m.EmojiPicker })),
);

interface EmojiPickerButtonProps {
  /** Receives the text to insert — a character, or a `<:name:hash>` token. */
  onSelect: (text: string) => void;
  /** Close the panel after one pick. Reactions want this; the composer does not. */
  closeOnSelect?: boolean;
  /**
   * Where the panel opens relative to the trigger. `up` is the composer case
   * (the trigger sits at the bottom of the window), `down` suits a toolbar.
   */
  placement?: "up" | "down";
  /** Horizontal edge the panel aligns to. */
  align?: "left" | "right";
  className?: string;
  "data-testid"?: string;
  ariaLabel?: string;
}

/**
 * The emoji-picker trigger plus its anchored panel — the one piece a caller
 * needs to mount.
 *
 * The panel is `absolute` inside this component's own `relative` wrapper: it is
 * anchored to the trigger, scrolls and unmounts with it, and is **not** a modal.
 * No portal to `body`, no `position: fixed`, no backdrop element — the same
 * shape the existing reaction picker uses. Outside clicks close it via a
 * `mousedown` listener rather than a click-catching overlay, so nothing is ever
 * covered by an invisible layer.
 */
export const EmojiPickerButton: React.FC<EmojiPickerButtonProps> = ({
  onSelect,
  closeOnSelect = false,
  placement = "up",
  align = "right",
  className = "",
  "data-testid": testId = "emoji-picker-button",
  ariaLabel,
}) => {
  const { t } = useTranslation("emoji");
  const [open, setOpen] = useState(false);
  const wrapperRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) {
      return;
    }
    const handlePointerDown = (event: MouseEvent) => {
      if (wrapperRef.current && !wrapperRef.current.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", handlePointerDown);
    return () => {
      document.removeEventListener("mousedown", handlePointerDown);
    };
  }, [open]);

  const label = ariaLabel ?? t("picker.triggerLabel");

  const positionClass = [
    placement === "up" ? "bottom-full mb-1" : "top-full mt-1",
    // `start`/`end`, not `left`/`right`: the panel is anchored to the edge
    // of the trigger that faces INTO the content, which swaps under RTL.
    align === "right" ? "end-0" : "start-0",
  ].join(" ");

  return (
    <div ref={wrapperRef} className={`relative ${className}`}>
      <button
        type="button"
        data-testid={testId}
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={label}
        onClick={() => setOpen((prev) => !prev)}
        className="icon-btn-sm"
        style={{ color: open ? "var(--c-accent)" : undefined }}
      >
        <Smile size={16} className="size-4 shrink-0" />
      </button>

      {open && (
        <div className={`absolute z-40 ${positionClass}`}>
          {/* `null` fallback rather than a spinner: the chunk is local and
              resolves within a frame or two, and a flashing placeholder where
              the grid is about to appear reads worse than the grid simply
              arriving. */}
          <Suspense fallback={null}>
            <EmojiPicker
              onSelect={onSelect}
              onClose={() => setOpen(false)}
              closeOnSelect={closeOnSelect}
            />
          </Suspense>
        </div>
      )}
    </div>
  );
};
