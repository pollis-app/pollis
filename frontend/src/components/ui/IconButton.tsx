import React from "react";
import type { ActivateOn } from "../../hooks/usePressActivation";
import { PressButton } from "./PressButton";
import { FOCUS_RING, FOCUS_RING_DANGER } from "./focusRing";

type IconButtonProps = Omit<React.ButtonHTMLAttributes<HTMLButtonElement>, "aria-label"> & {
  /** Required: an icon has no text of its own to name it. */
  "aria-label": string;
  /** `sm` (1.5rem) for a header or row action, `md` (2rem) for window chrome. */
  size?: "sm" | "md";
  /** `"press"` acts on mouse pointer-down (#1529); only for actions free to undo. Default `"click"`. */
  activateOn?: ActivateOn;
  /** A destructive action (delete, remove, disconnect): the danger focus ring, never the accent. */
  destructive?: boolean;
};

/**
 * An icon-only button (#1967): muted, accent and a hover fill under the pointer, the control
 * radius and the one focus ring. The look is the `.icon-btn` component class (index.css), so a
 * utility in `className` (a state colour, a position) always wins over it.
 */
export const IconButton = React.forwardRef<HTMLButtonElement, IconButtonProps>(
  ({ size = "sm", activateOn = "click", destructive = false, className = "", ...rest }, ref) => (
    <PressButton
      ref={ref}
      activateOn={activateOn}
      {...rest}
      className={`${size === "md" ? "icon-btn" : "icon-btn-sm"} ${destructive ? FOCUS_RING_DANGER : FOCUS_RING} ${className}`}
    />
  ),
);
IconButton.displayName = "IconButton";
