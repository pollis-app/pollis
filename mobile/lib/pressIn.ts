// Act on touch-down where an accidental press is free to undo (#1529):
// going back, closing a sheet, switching tabs, opening a menu. Anything that
// sends, persists, notifies others or confirms keeps acting on release, so
// the user can still slide off. Opt-in only, through the `pressIn` prop on
// the primitives in components/ui.tsx (and `usePressIn` for the few bare
// Pressables in fixed chrome). Kept free of react-native so the node tests
// can pin it.
//
// Never on a row inside a ScrollView / FlatList: touch-down lands before the
// scroll takes the touch, so every scroll that starts on a row would open it.
//
// Two activation paths, one action:
//  - A finger: onPressIn runs the action; the onPress that follows the
//    release of the same touch is swallowed.
//  - A screen reader: TalkBack's double tap is a click, which Pressable
//    turns into onPress with no onPressIn before it, so onPress runs the
//    action. (VoiceOver's double tap is a synthesised touch, so it takes the
//    finger path.) A touch that slides off and is cancelled gets no onPress;
//    its onPressOut clears the flag on the next tick, so it can never swallow
//    a later screen-reader activation.

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

// Android only: a control inside a Modal (every SheetOverlay, its Close and
// backdrop included) keeps acting on release. Closing the sheet, or
// navigating away, on touch-down dismisses the Modal's window while its touch
// is still down; that touch then never ends in JS, and the stale touch
// swallowed the next tap on the screen underneath (the first Back after a
// sheet's menu row did nothing). iOS ends the touch, so it keeps touch-down.
export function pressInSupported(platform: string, inSheet: boolean): boolean {
  return !(platform === "android" && inSheet);
}

// `pressIn` is refused on a control with a long press: the action would run
// before the hold could become a long press. Such a control keeps acting on
// release.
export function pressInAllowed(pressIn: boolean | undefined, hasLongPress: boolean): boolean {
  return !!pressIn && !hasLongPress;
}
