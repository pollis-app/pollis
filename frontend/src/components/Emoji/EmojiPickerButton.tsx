import React, { Suspense, lazy, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Smile, type LucideIcon } from "lucide-react";
import { type Placement, panelHeightPx, resolvePlacement } from "./pickerPlacement";

// The picker carries the full standard emoji table (`emojiData.ts`), which is
// the largest single application module in the bundle, plus the search index
// built on top of it. The panel is already mounted on demand, so none of that
// belongs in the startup chunk (#874). The trigger button — the only part
// always on screen — stays eager.
const EmojiPicker = lazy(() =>
  import("./EmojiPicker").then((m) => ({ default: m.EmojiPicker })),
);

/**
 * `EmojiPicker`'s own `h-[24rem]`, in rem.
 *
 * Duplicated from a Tailwind class, so `emoji-picker-placement.test.ts` reads
 * both and fails if they drift — a silently stale value here would resolve
 * placement against a panel height that no longer exists.
 */
const PANEL_REM_HEIGHT = 24;

interface EmojiPickerButtonProps {
  /** Receives the text to insert — a character, or a `<:name:hash>` token. */
  onSelect: (text: string) => void;
  /** Close the panel after one pick. Reactions want this; the composer does not. */
  closeOnSelect?: boolean;
  /**
   * The PREFERRED side. `up` is the composer case (the trigger normally sits at
   * the bottom of the window), `down` suits a toolbar.
   *
   * A preference, not an instruction: it is honoured whenever the panel fits
   * there, and overridden when it does not — see `pickerPlacement.ts`.
   */
  placement?: "up" | "down";
  /** Horizontal edge the panel aligns to. */
  align?: "left" | "right";
  className?: string;
  "data-testid"?: string;
  ariaLabel?: string;
  /** Trigger chrome. Defaults to the composer's `icon-btn-sm`; a message
   * hover bar passes its own button class so every action there matches. */
  buttonClassName?: string;
  /** Trigger glyph. Defaults to the plain smile. */
  icon?: LucideIcon;
  iconSize?: number;
  /** Defaults to `size-4`, which scales the glyph with the font setting. */
  iconClassName?: string;
  /** Marks the trigger for arrow-key message navigation (`data-nav-action`). */
  navAction?: string;
  /** Test id on the panel wrapper, for callers whose panel is not "the" picker. */
  panelTestId?: string;
  /** Reported on every open/close, so a host that hides on hover-out can hold
   * itself visible while the panel is up. */
  onOpenChange?: (open: boolean) => void;
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
  // Resolved when the panel opens, and again whenever the geometry it was
  // derived from moves. Seeded with the caller's preference so the very first
  // paint is never the wrong way round.
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

  // The decision is only as good as the geometry it was made from, and both
  // inputs move while the panel is open: resizing changes the space, and any
  // ancestor scrolling moves the trigger through it. `capture` because scroll
  // does not bubble — without it a scrolling message list would slide the
  // trigger up under a panel still opening the old way.
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
    // `start`/`end`, not `left`/`right`: the panel is anchored to the edge
    // of the trigger that faces INTO the content, which swaps under RTL.
    align === "right" ? "end-0" : "start-0",
  ].join(" ");

  return (
    <div ref={wrapperRef} className={`relative ${className}`}>
      <button
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
      </button>

      {open && (
        <div
          data-testid={panelTestId}
          data-placement={resolved.placement}
          className={`absolute z-40 ${positionClass}`}
        >
          {/* `null` fallback rather than a spinner: the chunk is local and
              resolves within a frame or two, and a flashing placeholder where
              the grid is about to appear reads worse than the grid simply
              arriving. */}
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
