// Act on touch-down where an accidental press is free to undo (#1529);
// anything that sends, persists, notifies others or confirms keeps acting on
// release, so the user can still slide off. Opt-in through `pressIn`. Never on
// a row inside a ScrollView / FlatList: touch-down lands before the scroll
// takes the touch, so every scroll that starts on a row would open it.
//
// Two activation paths, one action: a finger's onPressIn runs it and the
// onPress of the same touch is swallowed; TalkBack's double tap is an onPress
// with no onPressIn, so onPress runs it (VoiceOver synthesises a touch).

export type Schedule = (fn: () => void) => unknown;

export interface PressInGuard {
  onPressIn(action: () => void): void;
  onPress(action: () => void): void;
  onPressOut(): void;
}

export function createPressInGuard(
  schedule: Schedule = (fn) => setTimeout(fn, 0),
): PressInGuard {
  // A touch-down ran the action and its release has not been seen yet.
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
      // Pressable calls onPressOut just before onPress on a release (or up
      // to its minimum press duration after it), so the flag is cleared a
      // tick later: a release still sees it, a cancelled touch does not
      // leave it behind.
      schedule(() => {
        armed = false;
      });
    },
  };
}

// Android only: a control inside a Modal (every SheetOverlay) keeps acting on
// release. Dismissing the Modal on touch-down leaves that touch unfinished in
// JS, and it swallows the next tap on the screen underneath. iOS ends the
// touch, so it keeps touch-down.
export function pressInSupported(platform: string, inSheet: boolean): boolean {
  return !(platform === "android" && inSheet);
}

// Refused with a long press: the action would run before the hold could
// become one.
export function pressInAllowed(pressIn: boolean | undefined, hasLongPress: boolean): boolean {
  return !!pressIn && !hasLongPress;
}
