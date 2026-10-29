/** Arrow-key direction for keyboard nav: under RTL "next" is leftwards. Read from the element, since LTR islands exist (e.g. InputOtp). */

/** Horizontal arrow keys, and the direction each one points ON SCREEN. */
const SCREEN_DIRECTION: Record<string, 1 | -1> = {
  ArrowRight: 1,
  ArrowLeft: -1,
};

export function isHorizontalArrow(key: string): boolean {
  return key in SCREEN_DIRECTION;
}

/** Horizontal arrow → item-order step (+1 next, -1 previous, 0 other). */
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
    return false;
  }
}

/** Item-order step for a horizontal arrow on `el`; 0 if not one. */
export function horizontalArrowStep(
  key: string,
  el: Element | null | undefined,
): 0 | 1 | -1 {
  return arrowStep(key, isRtlElement(el));
}
