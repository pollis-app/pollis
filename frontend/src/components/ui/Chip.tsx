import React from "react";
import { FOCUS_RING } from "./focusRing";

interface ChipProps {
  children: React.ReactNode;
  onClick?: () => void;
  /** The chosen one of a set, or a chip in its active state: the accent edge on the `active` tint. */
  selected?: boolean;
  disabled?: boolean;
  title?: string;
  "aria-label"?: string;
  /** Toggle state for assistive tech, when the chip is a choice. */
  "aria-pressed"?: boolean;
  "data-testid"?: string;
  /** Machine text (a key combo) reads in `font-machine`. */
  machine?: boolean;
  /** Runtime colours only: a preset chip previews its own colour. */
  style?: Pick<React.CSSProperties, "background" | "borderColor" | "color">;
}

/**
 * A small pressable chip (#1967): a preset, a remembered account, a key combo to rebind. The chip
 * radius, a `borderStrong` edge, and the one focus ring; `selected` is the accent edge on the
 * `active` tint, never a solid accent fill (docs/design/README.md).
 */
export const Chip: React.FC<ChipProps> = ({
  children,
  onClick,
  selected = false,
  disabled = false,
  title,
  "aria-label": ariaLabel,
  "aria-pressed": ariaPressed,
  "data-testid": testId,
  machine = false,
  style,
}) => (
  <button
    type="button"
    onClick={onClick}
    disabled={disabled}
    title={title}
    aria-label={ariaLabel}
    aria-pressed={ariaPressed}
    data-testid={testId}
    className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-chip border text-secondary text-fg transition-colors enabled:cursor-pointer disabled:cursor-default disabled:opacity-60 ${
      machine ? "font-machine" : "font-mono"
    } ${selected ? "border-accent bg-active" : "border-line-strong bg-transparent enabled:hover:bg-hover"} ${FOCUS_RING}`}
    style={style}
  >
    {children}
  </button>
);
