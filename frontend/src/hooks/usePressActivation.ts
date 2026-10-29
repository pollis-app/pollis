import { useRef } from "react";
import type React from "react";
import { createPressGuard, type PressGuard } from "../utils/pressActivation";

export type ActivateOn = "click" | "press";

export interface PressActivationHandlers<E extends Element> {
  onClick?: (e: React.MouseEvent<E>) => void;
  onPointerDown?: (e: React.PointerEvent<E>) => void;
  onMouseDown?: (e: React.MouseEvent<E>) => void;
}

type Activator = <E extends Element = HTMLButtonElement>(
  onClick: ((e: React.MouseEvent<E>) => void) | undefined,
  activateOn: ActivateOn | undefined,
  disabled?: boolean,
) => PressActivationHandlers<E>;

// Handlers for opt-in pointer-down activation (#1529; rules in
// utils/pressActivation.ts), spread onto a button or row. With `activateOn`
// "click" (the default) they are the plain `onClick`. With "press" a plain
// primary mouse press acts on pointerdown and its click is swallowed, while
// the keyboard, assistive tech, touch, pen and modified clicks still act on
// click — so the action runs exactly once whichever way it is activated.
//
// If the action moved focus (it opened a panel whose field autofocuses), the
// press's own mousedown would hand focus back to the pressed element; that
// default is prevented. Otherwise mousedown is untouched, so the element
// still takes focus as any click would.
//
// One activator serves any number of elements (the rows of a list): only one
// press is ever in flight, so they share a single guard.
export function usePressActivator(): Activator {
  const guard = useRef<PressGuard | null>(null);
  const focusBefore = useRef<Element | null>(null);
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
        focusBefore.current = document.activeElement;
        const acted = g.pointerDown(e, () => onClick(e));
        if (acted) {
          // The click that ends this press comes in the same task as its
          // pointerup; if the release lands elsewhere there is no click, so
          // forget the press a tick after the pointer comes up.
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
        if (g.armed && document.activeElement !== focusBefore.current) {
          e.preventDefault();
        }
      },
      onClick: (e) => g.click(e, () => onClick(e)),
    };
  };
}

// The single-element form of `usePressActivator`.
export function usePressActivation<E extends Element = HTMLButtonElement>(
  onClick: ((e: React.MouseEvent<E>) => void) | undefined,
  activateOn: ActivateOn | undefined,
  disabled?: boolean,
): PressActivationHandlers<E> {
  return usePressActivator()<E>(onClick, activateOn, disabled);
}
