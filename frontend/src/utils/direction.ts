/**
 * Reading direction for keyboard navigation.
 *
 * `ArrowRight` is not "next". It is "rightwards". Under `dir="rtl"` the next
 * item in a list or grid is drawn to the LEFT, so a handler that hardcodes
 * `ArrowRight: +1` walks an Arabic user backwards through their own UI. See
 * the RTL section of `.codesight/wiki/i18n.md`.
 *
 * Vertical arrows need none of this: `dir` mirrors the inline axis only, and
 * the block axis is top-to-bottom in every locale Pollis ships.
 *
 * ## Why the ELEMENT and not the language
 *
 * Direction is resolved from the focused element's own computed style, not
 * from `i18n.language` or `<html dir>`. The app deliberately contains LTR
 * islands inside an RTL page — `ui/InputOtp.tsx` pins its digit row to
 * `dir="ltr"` so the boxes do not reverse, and its key handler depends on
 * ArrowLeft still meaning "previous box" there. Asking the element gets that
 * right for free; asking the language would break it.
 */

/** Horizontal arrow keys, and the direction each one points ON SCREEN. */
const SCREEN_DIRECTION: Record<string, 1 | -1> = {
  ArrowRight: 1,
  ArrowLeft: -1,
};

export function isHorizontalArrow(key: string): boolean {
  return key in SCREEN_DIRECTION;
}

/**
 * Resolve a horizontal arrow key to a signed step in ITEM order, given the
 * reading direction: `+1` = next, `-1` = previous, `0` = not a horizontal
 * arrow.
 *
 * Split out from the DOM lookup below so the mapping itself is unit-testable
 * without a browser (`frontend/tests/direction.test.ts`).
 */
export function arrowStep(key: string, rtl: boolean): 0 | 1 | -1 {
  const screen = SCREEN_DIRECTION[key];
  if (screen === undefined) {
    return 0;
  }
  return rtl ? ((-screen) as 1 | -1) : screen;
}

/** Is this element laid out right-to-left? */
export function isRtlElement(el: Element | null | undefined): boolean {
  if (!el) {
    return false;
  }
  try {
    return getComputedStyle(el).direction === "rtl";
  } catch {
    // No layout engine (SSR, a detached node) — LTR is the safe default and
    // matches the behaviour this replaced.
    return false;
  }
}

/**
 * The signed step a horizontal arrow key should produce for navigation
 * anchored on `el`. `0` means "not a horizontal arrow" — let the caller's
 * other cases handle it.
 */
export function horizontalArrowStep(
  key: string,
  el: Element | null | undefined,
): 0 | 1 | -1 {
  return arrowStep(key, isRtlElement(el));
}
