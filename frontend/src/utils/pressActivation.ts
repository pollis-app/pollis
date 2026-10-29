// Opt-in pointer-down activation for undoable actions (#1529): a plain primary mouse press acts on pointerdown and swallows its click; everything else acts on click.

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
  // Forget the press: released elsewhere, so no click to swallow.
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
