import React, { useEffect, useRef, useState } from "react";
import { Smile } from "lucide-react";
import { EmojiPicker } from "./EmojiPicker";

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
  ariaLabel = "Emoji",
}) => {
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

  const positionClass = [
    placement === "up" ? "bottom-full mb-1" : "top-full mt-1",
    align === "right" ? "right-0" : "left-0",
  ].join(" ");

  return (
    <div ref={wrapperRef} className={`relative ${className}`}>
      <button
        type="button"
        data-testid={testId}
        aria-label={ariaLabel}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={ariaLabel}
        onClick={() => setOpen((prev) => !prev)}
        className="icon-btn-sm"
        style={{ color: open ? "var(--c-accent)" : undefined }}
      >
        <Smile size={16} className="size-4 shrink-0" />
      </button>

      {open && (
        <div className={`absolute z-40 ${positionClass}`}>
          <EmojiPicker
            onSelect={onSelect}
            onClose={() => setOpen(false)}
            closeOnSelect={closeOnSelect}
          />
        </div>
      )}
    </div>
  );
};
