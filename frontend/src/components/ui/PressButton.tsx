import React from "react";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

type PressButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** `"press"` (the default here): see `utils/pressActivation.ts` (#1529). */
  activateOn?: ActivateOn;
};

/**
 * An unstyled `<button>` that acts on pointer-down (#1529), for bare chrome
 * buttons whose accidental press is free to undo. Never for sending, saving,
 * confirming, destroying or toggling.
 */
export const PressButton = React.forwardRef<HTMLButtonElement, PressButtonProps>(
  ({ activateOn = "press", onClick, onPointerDown, onMouseDown, type = "button", ...rest }, ref) => {
    const press = usePressActivation<HTMLButtonElement>(onClick, activateOn, rest.disabled);
    return (
      <button
        ref={ref}
        type={type}
        {...rest}
        onClick={press.onClick}
        onPointerDown={(e) => {
          onPointerDown?.(e);
          press.onPointerDown?.(e);
        }}
        onMouseDown={(e) => {
          onMouseDown?.(e);
          press.onMouseDown?.(e);
        }}
      />
    );
  },
);
PressButton.displayName = "PressButton";
