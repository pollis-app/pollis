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

// Opt-in pointer-down activation (#1529; rules in utils/pressActivation.ts): runs exactly once however activated.
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
        // A nested control acts on its own click.
        const inner = (e.target as Element).closest(NESTED_CONTROL);
        if (inner && inner !== e.currentTarget && e.currentTarget.contains(inner)) {
          return;
        }
        const acted = g.pointerDown(e, () => onClick(e));
        if (acted) {
          // No click if released elsewhere, so forget the press a tick after pointerup.
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
        // Prevent the mousedown default so focus stays where the action put it.
        if (g.armed) {
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
