import React from "react";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

type PressButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** Default `"press"`: a mouse press acts on pointer-down (#1529). */
  activateOn?: ActivateOn;
};

/**
 * The unstyled `<button>` for bare chrome, whole rows and grid cells, whose look is their own.
 * By default a mouse press acts on pointer-down (#1529), for presses free to undo; a row or cell
 * that sends, saves, confirms, destroys or toggles passes `activateOn="click"`.
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
