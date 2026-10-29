// Swipe to reply (#1270): the numbers and drag curve behind
// components/chat/SwipeToReply.tsx, free of react-native for the node tests.
// The row follows the finger 1:1 to the threshold (reply armed), then drags with
// resistance up to MAX. Functions are worklets, so the curve costs no JS-thread
// work per frame.

export const SWIPE_REPLY = {
  // Small enough to feel immediate, large enough that a vertical scroll that
  // wobbles sideways never starts a swipe.
  activateX: 12,
  // Vertical travel that hands the touch back to the list.
  failY: 10,
  threshold: 64,
  max: 96,
  // Share of the finger's travel the row follows past the threshold.
  resistance: 0.35,
  // The double-tap fallback's nudge.
  nudge: 36,
  nudgeMs: 110,
  doubleTapMs: 250,
} as const;

export const SWIPE_SPRING = { damping: 18, stiffness: 240, mass: 0.6 } as const;

// Screen-space sign of a swipe toward the start edge. Gesture translation and
// transforms are screen space, which Yoga's RTL mirroring does not touch.
export function replyDirection(isRTL: boolean): -1 | 1 {
  return isRTL ? 1 : -1;
}

// Never negative: a drag the other way does not move the row.
export function towardStart(translationX: number, dir: -1 | 1): number {
  "worklet";
  return Math.max(0, translationX * dir);
}

export function dragDistance(travel: number): number {
  "worklet";
  const { threshold, max, resistance } = SWIPE_REPLY;
  if (travel <= threshold) {
    return travel;
  }
  return Math.min(max, threshold + (travel - threshold) * resistance);
}

export function isArmed(travel: number): boolean {
  "worklet";
  return travel >= SWIPE_REPLY.threshold;
}

export function arrowProgress(distance: number): number {
  "worklet";
  return Math.min(1, Math.max(0, distance / SWIPE_REPLY.threshold));
}

// Activates only toward the start edge, and fails on a drag the other way (the
// iOS and Android back gestures go there) or on vertical travel.
export function panRanges(dir: -1 | 1): {
  activeOffsetX: [number, number];
  failOffsetX: [number, number];
  failOffsetY: [number, number];
} {
  const { activateX, failY } = SWIPE_REPLY;
  const far = 10_000;
  return dir < 0
    ? {
        activeOffsetX: [-activateX, far],
        failOffsetX: [-far, activateX],
        failOffsetY: [-failY, failY],
      }
    : {
        activeOffsetX: [-far, activateX],
        failOffsetX: [-activateX, far],
        failOffsetY: [-failY, failY],
      };
}
