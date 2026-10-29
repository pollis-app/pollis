import React from "react";
import { usePressActivation, type ActivateOn } from "../../hooks/usePressActivation";

type PressButtonProps = React.ButtonHTMLAttributes<HTMLButtonElement> & {
  /** Default `"press"`: a mouse press acts on pointer-down (#1529). */
  activateOn?: ActivateOn;
};

/** Unstyled `<button>` acting on pointer-down (#1529), for presses free to undo. Never send/save/confirm/destroy/toggle. */
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
