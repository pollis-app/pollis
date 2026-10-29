/**
 * Reading direction for keyboard navigation: under `dir="rtl"` the next item is
 * to the LEFT. Resolved from the focused element's computed style, not the
 * language, because LTR islands exist (`ui/InputOtp.tsx`). Vertical arrows
 * never mirror. See `.codesight/wiki/i18n.md`.
 */

const SCREEN_DIRECTION: Record<string, 1 | -1> = {
  ArrowRight: 1,
  ArrowLeft: -1,
};

export function isHorizontalArrow(key: string): boolean {
  return key in SCREEN_DIRECTION;
}

/**
 * A horizontal arrow as a step in ITEM order (`+1` next, `-1` previous, `0` not
 * horizontal). DOM-free for `direction.test.ts`.
 */
export function arrowStep(key: string, rtl: boolean): 0 | 1 | -1 {
  const screen = SCREEN_DIRECTION[key];
  if (screen === undefined) {
    return 0;
  }
  return rtl ? ((-screen) as 1 | -1) : screen;
}

export function isRtlElement(el: Element | null | undefined): boolean {
  if (!el) {
    return false;
  }
  try {
    return getComputedStyle(el).direction === "rtl";
  } catch {
      // No layout engine (SSR, detached node): LTR.
    return false;
  }
}

/** The item-order step for a horizontal arrow on `el`; `0` for other keys. */
export function horizontalArrowStep(
  key: string,
  el: Element | null | undefined,
): 0 | 1 | -1 {
  return arrowStep(key, isRtlElement(el));
}
