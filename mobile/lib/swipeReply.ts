// Swipe to reply (#1270): numbers and drag curve, pure; every function also runs as a worklet.

export const SWIPE_REPLY = {
  // Small enough to feel immediate, large enough that a wobbly vertical scroll never starts a swipe.
  activateX: 12,
  failY: 10,
  threshold: 64,
  max: 96,
  resistance: 0.35,
  nudge: 36,
  nudgeMs: 110,
  doubleTapMs: 250,
} as const;

export const SWIPE_SPRING = { damping: 18, stiffness: 240, mass: 0.6 } as const;

// -1 (left) in LTR, +1 in RTL: gesture space is not mirrored by Yoga.
export function replyDirection(isRTL: boolean): -1 | 1 {
  return isRTL ? 1 : -1;
}

// Never negative: a drag the other way does not move the row.
export function towardStart(translationX: number, dir: -1 | 1): number {
  "worklet";
  return Math.max(0, translationX * dir);
}

// 1:1 up to the threshold, then damped, never past MAX.
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

// Fails on a drag the other way, where the system back gestures go.
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
