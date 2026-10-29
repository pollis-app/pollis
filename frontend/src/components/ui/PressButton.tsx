import React from "react";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

type PressButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** `"press"` (the default here): a plain mouse press acts on pointer-down
      (#1529); the keyboard, assistive tech and modified clicks act on click,
      and a mouse press runs the action once. */
  activateOn?: ActivateOn;
};

/**
 * An unstyled `<button>` that acts on pointer-down (#1529), for the bare
 * buttons in fixed chrome whose accidental press is free to undo — opening a
 * panel, a viewer or a page, jumping to a message. It renders exactly the
 * button it is given (classes, attributes, children); only the activation
 * changes. Use `Button activateOn="press"` where the styled button is wanted.
 * Never for sending, saving, confirming, destroying or toggling.
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
