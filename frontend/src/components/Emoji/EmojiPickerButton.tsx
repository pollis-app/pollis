import React, { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Smile, type LucideIcon } from "lucide-react";
import { type Placement, panelHeightPx, resolvePlacement } from "./pickerPlacement";
import { PressButton } from "../ui/PressButton";

// The picker carries the full emoji table, the largest app module, so it stays
// out of the startup chunk (#874).
const EmojiPicker = lazy(() =>
  import("./EmojiPicker").then((m) => ({ default: m.EmojiPicker })),
);

/** `EmojiPicker`'s `h-[24rem]`; `emoji-picker-placement.test.ts` fails on drift. */
const PANEL_REM_HEIGHT = 24;

interface EmojiPickerButtonProps {
  /** Receives the text to insert — a character, or a `<:name:hash>` token. */
  onSelect: (text: string) => void;
  closeOnSelect?: boolean;
  /**
   * The PREFERRED side, honoured when the panel fits (`pickerPlacement.ts`).
   * `up` suits the composer, `down` a toolbar.
   */
  placement?: "up" | "down";
  align?: "left" | "right";
  className?: string;
  "data-testid"?: string;
  ariaLabel?: string;
  /** Trigger chrome; defaults to the composer's `icon-btn-sm`. */
  buttonClassName?: string;
  icon?: LucideIcon;
  iconSize?: number;
  iconClassName?: string;
  navAction?: string;
  panelTestId?: string;
  /** Lets a host that hides on hover-out stay visible while the panel is up. */
  onOpenChange?: (open: boolean) => void;
}

/**
 * The emoji-picker trigger plus its anchored panel. The panel is `absolute` in
 * this component's `relative` wrapper (no portal, fixed position or backdrop);
 * outside clicks close it via a `mousedown` listener.
 */
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
  // Seeded with the caller's preference so the first paint is never reversed.
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

  // Re-resolve on resize and on any ancestor scroll while open. `capture`
  // because scroll doesn't bubble.
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
    // Logical edges: the trigger's content-facing edge swaps under RTL.
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
