// Act on pointer-down where an accidental press is free to undo (#1529):
// navigating, opening a panel or menu, switching a view, expanding a section.
// Anything that sends, persists, notifies others or confirms keeps acting on
// click (release), so the user can still drag off. Opt-in only, through
// `activateOn="press"` on `Button`, the Sidebar `Row` / `SectionHeader`,
// `TerminalMenu` items and `NavigableList`, or `usePressActivation` for a bare
// button. Kept free of React and the DOM so the node tests can pin it.
//
// Two activation paths, one action:
//  - A plain primary mouse press (`pointerType === "mouse"`, `button === 0`,
//    no modifier keys) runs the action on pointerdown; the click that follows
//    the release of the same press is swallowed.
//  - Everything else runs it on click: the keyboard and assistive tech
//    (Enter / Space / VoiceOver produce a click with `detail === 0`), touch
//    and pen, and a modified click (Cmd/Ctrl/Shift/Alt keep their meaning).

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
  // Forget the press: its release landed elsewhere (dragged off, or the
  // control unmounted), so there is no click to swallow.
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
