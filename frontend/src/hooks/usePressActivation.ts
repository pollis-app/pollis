import { useRef } from "react";
import type React from "react";
import { createPressGuard, type PressGuard } from "../utils/pressActivation";

export type ActivateOn = "click" | "press";

export interface PressActivationHandlers<E extends Element> {
  onClick?: (e: React.MouseEvent<E>) => void;
  onPointerDown?: (e: React.PointerEvent<E>) => void;
  onMouseDown?: (e: React.MouseEvent<E>) => void;
}

// Controls that own their own activation when nested inside a pressable row.
const NESTED_CONTROL = 'button, a[href], input, select, textarea, [role="switch"], [role="button"]';

type Activator = <E extends Element = HTMLButtonElement>(
  onClick: ((e: React.MouseEvent<E>) => void) | undefined,
  activateOn: ActivateOn | undefined,
  disabled?: boolean,
) => PressActivationHandlers<E>;

// Handlers for opt-in pointer-down activation (#1529; rules in
// utils/pressActivation.ts), spread onto a button or row. One activator serves
// many elements: only one press is ever in flight, so they share one guard.
export function usePressActivator(): Activator {
  const guard = useRef<PressGuard | null>(null);
  if (!guard.current) {
    guard.current = createPressGuard();
  }
  const g = guard.current;
  return (onClick, activateOn, disabled) => {
    if (activateOn !== "press" || !onClick || disabled) {
      return { onClick };
    }
    return {
      onPointerDown: (e) => {
        // A press on a nested control belongs to that control.
        const inner = (e.target as Element).closest(NESTED_CONTROL);
        if (inner && inner !== e.currentTarget && e.currentTarget.contains(inner)) {
          return;
        }
        const acted = g.pointerDown(e, () => onClick(e));
        if (acted) {
          // A release elsewhere yields no click, so disarm a tick after
          // pointerup (the press's own click arrives in the same task).
          const release = () => {
            window.removeEventListener("pointerup", release, true);
            window.removeEventListener("pointercancel", release, true);
            setTimeout(() => g.disarm(), 0);
          };
          window.addEventListener("pointerup", release, true);
          window.addEventListener("pointercancel", release, true);
        }
      },
      onMouseDown: (e) => {
        // The action may have moved focus; the press's mousedown default
        // would hand it back to the pressed element.
        if (g.armed) {
          e.preventDefault();
        }
      },
      onClick: (e) => g.click(e, () => onClick(e)),
    };
  };
}

export function usePressActivation<E extends Element = HTMLButtonElement>(
  onClick: ((e: React.MouseEvent<E>) => void) | undefined,
  activateOn: ActivateOn | undefined,
  disabled?: boolean,
): PressActivationHandlers<E> {
  return usePressActivator()<E>(onClick, activateOn, disabled);
}
