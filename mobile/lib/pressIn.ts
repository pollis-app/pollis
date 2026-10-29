// Touch-down activation where an accidental press is free to undo (#1529). Never in a scroller. onPress stays the screen-reader path; a guard runs the action once.

export type Schedule = (fn: () => void) => unknown;

export interface PressInGuard {
  onPressIn(action: () => void): void;
  onPress(action: () => void): void;
  onPressOut(): void;
}

export function createPressInGuard(
  schedule: Schedule = (fn) => setTimeout(fn, 0),
): PressInGuard {
  let armed = false;
  return {
    onPressIn(action) {
      armed = true;
      action();
    },
    onPress(action) {
      if (armed) {
        armed = false;
        return;
      }
      action();
    },
    onPressOut() {
      // onPressOut precedes onPress on a release, so clear a tick later.
      schedule(() => {
        armed = false;
      });
    },
  };
}

// Android: in a Modal, touch-down dismissal left the touch unfinished and swallowed the next tap.
export function pressInSupported(platform: string, inSheet: boolean): boolean {
  return !(platform === "android" && inSheet);
}

// Refused with a long press: the action would run before the hold.
export function pressInAllowed(pressIn: boolean | undefined, hasLongPress: boolean): boolean {
  return !!pressIn && !hasLongPress;
}
