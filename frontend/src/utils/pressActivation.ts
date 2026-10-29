// Act on pointer-down only where an accidental press is free to undo (#1529):
// navigating, opening a panel, switching a view. Anything that sends, persists
// or confirms keeps acting on click, so the user can still drag off. Opt-in via
// `activateOn="press"`. Pure so node tests can pin it.
//
// A plain primary mouse press acts on pointerdown and its click is swallowed;
// everything else (keyboard, assistive tech, touch, pen, modified clicks) acts
// on click, so the action runs exactly once.

export interface PressLike {
  pointerType: string;
  button: number;
  ctrlKey: boolean;
  metaKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

export function isPlainPrimaryMousePress(e: PressLike): boolean {
  return (
    e.pointerType === "mouse" &&
    e.button === 0 &&
    !e.ctrlKey &&
    !e.metaKey &&
    !e.shiftKey &&
    !e.altKey
  );
}

export interface PressGuard {
  // Returns true when the press ran the action.
  pointerDown(e: PressLike, action: () => void): boolean;
  click(e: { detail: number }, action: () => void): void;
  // A press ran the action and its click has not been seen yet.
  readonly armed: boolean;
  // Forget the press: its release landed elsewhere, so no click will follow.
  disarm(): void;
}

export function createPressGuard(): PressGuard {
  let armed = false;
  return {
    pointerDown(e, action) {
      if (!isPlainPrimaryMousePress(e)) {
        armed = false;
        return false;
      }
      armed = true;
      action();
      return true;
    },
    click(e, action) {
      // detail 0 = not a pointer click (keyboard, assistive tech): always act.
      if (e.detail === 0) {
        action();
        return;
      }
      if (armed) {
        armed = false;
        return;
      }
      action();
    },
    get armed() {
      return armed;
    },
    disarm() {
      armed = false;
    },
  };
}
