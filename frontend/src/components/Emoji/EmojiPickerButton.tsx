import React, { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Smile, type LucideIcon } from "lucide-react";
import { type Placement, panelHeightPx, resolvePlacement } from "./pickerPlacement";
import { PressButton } from "../ui/PressButton";

// Lazy: the picker pulls in the largest module (`emojiData.ts`) (#874).
const EmojiPicker = lazy(() =>
  import("./EmojiPicker").then((m) => ({ default: m.EmojiPicker })),
);

/** EmojiPicker's `h-[24rem]`; emoji-picker-placement.test.ts fails on drift. */
const PANEL_REM_HEIGHT = 24;

interface EmojiPickerButtonProps {
  onSelect: (text: string) => void;
  closeOnSelect?: boolean;
  /** Preferred side, overridden when it doesn't fit (see `pickerPlacement.ts`). */
  placement?: "up" | "down";
  align?: "left" | "right";
  className?: string;
  "data-testid"?: string;
  ariaLabel?: string;
  buttonClassName?: string;
  icon?: LucideIcon;
  iconSize?: number;
  iconClassName?: string;
  /** Marks the trigger for arrow-key navigation. */
  navAction?: string;
  panelTestId?: string;
  /** Lets a hover-revealed host stay visible while the panel is up. */
  onOpenChange?: (open: boolean) => void;
}

/** Trigger plus a panel anchored `absolute` to it: no portal, no backdrop; outside clicks close via `mousedown`. */
export const EmojiPickerButton: React.FC<EmojiPickerButtonProps> = ({
  onSelect,
  closeOnSelect = false,
  placement = "up",
  align = "right",
  className = "",
  "data-testid": testId = "emoji-picker-button",
  ariaLabel,
  buttonClassName = "icon-btn-sm",
  icon: Icon = Smile,
  iconSize = 16,
  iconClassName = "size-4 shrink-0",
  navAction,
  panelTestId,
  onOpenChange,
}) => {
  const { t } = useTranslation("emoji");
  const [open, setOpenState] = useState(false);
  const setOpen = useCallback(
    (next: boolean) => {
      setOpenState(next);
      onOpenChange?.(next);
    },
    [onOpenChange],
  );
  // Seeded with the preference so the first paint isn't flipped.
  const [resolved, setResolved] = useState<Placement>({ placement });
  const wrapperRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const measure = useCallback(() => {
    const rect = triggerRef.current?.getBoundingClientRect();
    if (!rect) {
      return;
    }
    setResolved(
      resolvePlacement({
        preferred: placement,
        spaceAbove: rect.top,
        spaceBelow: window.innerHeight - rect.bottom,
        desired: panelHeightPx(PANEL_REM_HEIGHT),
      }),
    );
  }, [placement]);

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
  }, [open, setOpen]);

  // Re-resolve on resize and any ancestor scroll (`capture`: scroll doesn't bubble).
  useEffect(() => {
    if (!open) {
      return;
    }
    measure();
    window.addEventListener("resize", measure);
    window.addEventListener("scroll", measure, true);
    return () => {
      window.removeEventListener("resize", measure);
      window.removeEventListener("scroll", measure, true);
    };
  }, [open, measure]);

  const label = ariaLabel ?? t("picker.triggerLabel");

  const positionClass = [
    resolved.placement === "up" ? "bottom-full mb-1" : "top-full mt-1",
    // Logical edges: the facing edge swaps under RTL.
    align === "right" ? "end-0" : "start-0",
  ].join(" ");

  return (
    <div ref={wrapperRef} className={`relative ${className}`}>
      <PressButton
        ref={triggerRef}
        type="button"
        data-testid={testId}
        aria-label={label}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={label}
        data-nav-action={navAction}
        onClick={() => setOpen(!open)}
        className={buttonClassName}
        style={{ color: open ? "var(--c-accent)" : undefined }}
      >
        <Icon size={iconSize} className={iconClassName} />
      </PressButton>

      {open && (
        <div
          data-testid={panelTestId}
          data-placement={resolved.placement}
          className={`absolute z-40 ${positionClass}`}
        >
          {/* No spinner: the local chunk resolves within a frame or two. */}
          <Suspense fallback={null}>
            <EmojiPicker
              onSelect={onSelect}
              onClose={() => setOpen(false)}
              closeOnSelect={closeOnSelect}
              maxHeight={resolved.maxHeight}
            />
          </Suspense>
        </div>
      )}
    </div>
  );
};
