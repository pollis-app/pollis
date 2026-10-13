// Swipe to reply (#1270): the numbers and the drag curve behind
// components/chat/SwipeToReply.tsx, kept free of react-native so the node
// tests can pin them.
//
// Discord's gesture: drag a message toward the START edge (left in LTR,
// right in RTL). The row follows the finger 1:1 up to the threshold, where
// the reply arrow is fully grown and the reply is armed; past it the row
// drags with resistance and stops at MAX. Releasing while armed opens the
// reply bar, and the row springs home either way.
//
// Every function here also runs as a worklet on the UI thread (the
// "worklet" directive), so the curve costs no JS-thread work per frame.

export const SWIPE_REPLY = {
  // Horizontal travel before the pan claims the touch. Small enough to feel
  // immediate, large enough that a vertical scroll that wobbles sideways
  // never starts a swipe.
  activateX: 12,
  // Vertical travel that hands the touch back to the list.
  failY: 10,
  // Travel at which the reply arms.
  threshold: 64,
  // How far the row can be dragged at all.
  max: 96,
  // Share of the finger's travel the row follows past the threshold.
  resistance: 0.35,
  // The double-tap fallback's nudge toward the start edge.
  nudge: 36,
  nudgeMs: 110,
  // Two taps must land this close together to count as a double tap.
  doubleTapMs: 250,
} as const;

// The release spring: quick, lightly damped, settles with no visible wobble.
export const SWIPE_SPRING = { damping: 18, stiffness: 240, mass: 0.6 } as const;

// Screen-space sign of a swipe toward the start edge: -1 (left) in LTR,
// +1 (right) in RTL. Gesture translation and transforms are both in screen
// space, which Yoga's RTL mirroring does not touch.
export function replyDirection(isRTL: boolean): -1 | 1 {
  return isRTL ? 1 : -1;
}

// How far the finger has travelled toward the start edge (never negative: a
// drag the other way does not move the row).
export function towardStart(translationX: number, dir: -1 | 1): number {
  "worklet";
  return Math.max(0, translationX * dir);
}

// Distance the row is drawn at for `travel` toward the start edge: 1:1 up to
// the threshold, then damped, and never past MAX.
export function dragDistance(travel: number): number {
  "worklet";
  const { threshold, max, resistance } = SWIPE_REPLY;
  if (travel <= threshold) {
    return travel;
  }
  return Math.min(max, threshold + (travel - threshold) * resistance);
}

// Whether releasing now sends the reply.
export function isArmed(travel: number): boolean {
  "worklet";
  return travel >= SWIPE_REPLY.threshold;
}

// 0..1: how grown the reply arrow is at a drawn distance.
export function arrowProgress(distance: number): number {
  "worklet";
  return Math.min(1, Math.max(0, distance / SWIPE_REPLY.threshold));
}

// The pan's activation and failure ranges for a direction, in Gesture
// Handler's [min, max] form. It activates only on travel toward the start
// edge, and fails on a drag the other way (the iOS back swipe and Android's
// system back gesture both go there) or on vertical travel.
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
