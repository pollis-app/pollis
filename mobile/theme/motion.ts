// Motion (#2125): the shared timings and curve from design/theme.ts, as Reanimated builders.
// Rules: docs/design/README.md → Motion. Every movement here has a reduced-motion form (an opacity
// fade or nothing), picked with `useReduceMotion()` (hooks/useReduceMotion.ts).

import {
  Easing,
  FadeIn,
  FadeOut,
  Keyframe,
  LinearTransition,
  ReduceMotion,
} from "react-native-reanimated";
import { MOTION } from "../../design/theme.ts";

export { MOTION };

/** The one ease-out curve. */
export const EASE_OUT = Easing.bezier(...MOTION.easeOut);
/** Its mirror, for an exit that plays an entrance backwards. */
export const EASE_IN = Easing.bezier(...MOTION.easeIn);

/** What arrives after its row painted (a pill row, a custom emoji image) fades in within its space (#2115). Skipped under Reduce Motion. */
export const LATE_FADE_IN = FadeIn.duration(MOTION.fastMs).reduceMotion(ReduceMotion.System);

/** The reduced-motion stand-in for a movement: a short opacity fade, played even under Reduce Motion. */
export const REDUCED_FADE_IN = FadeIn.duration(MOTION.fastMs).reduceMotion(ReduceMotion.Never);

/**
 * A new message rises `messageRisePx` and fades in. `inverted`: the row sits in an inverted list,
 * whose cells are flipped, so "from below" is a negative offset in the row's own coordinates.
 */
export function messageRiseIn(inverted: boolean) {
  const from = (inverted ? -1 : 1) * MOTION.messageRisePx;
  return new Keyframe({
    0: { opacity: 0, transform: [{ translateY: from }] },
    100: { opacity: 1, transform: [{ translateY: 0 }], easing: EASE_OUT },
  })
    .duration(MOTION.baseMs)
    .reduceMotion(ReduceMotion.Never);
}

/** A new reaction pill or unread badge: grows from `popFromScale`, peaks at `popPeakScale`, settles at 1. */
export const POP_IN = new Keyframe({
  0: { opacity: 0, transform: [{ scale: MOTION.popFromScale }] },
  70: { opacity: 1, transform: [{ scale: MOTION.popPeakScale }], easing: EASE_OUT },
  100: { opacity: 1, transform: [{ scale: 1 }], easing: EASE_OUT },
})
  .duration(MOTION.baseMs)
  .reduceMotion(ReduceMotion.Never);

/** A changed count rolls up into place from below its line. */
export const ROLL_IN = new Keyframe({
  0: { opacity: 0, transform: [{ translateY: MOTION.messageRisePx }] },
  100: { opacity: 1, transform: [{ translateY: 0 }], easing: EASE_OUT },
})
  .duration(MOTION.baseMs)
  .reduceMotion(ReduceMotion.Never);

/** Rows under a folding category slide to their new place. */
export const FOLD_LAYOUT = LinearTransition.duration(MOTION.baseMs)
  .easing(EASE_OUT)
  .reduceMotion(ReduceMotion.Never);
/** A row a category shows as it opens… */
export const FOLD_IN = FadeIn.duration(MOTION.baseMs).reduceMotion(ReduceMotion.Never);
/** …or hides as it folds. */
export const FOLD_OUT = FadeOut.duration(MOTION.fastMs).reduceMotion(ReduceMotion.Never);

/** Content that replaces a loading or empty state fades in over it. */
export const CONTENT_FADE_IN = FadeIn.duration(MOTION.baseMs).reduceMotion(ReduceMotion.Never);

/** Timing config for a UI-thread `withTiming` that follows the curve; Reduce Motion handled by the caller. */
export const BASE_TIMING = { duration: MOTION.baseMs, easing: EASE_OUT, reduceMotion: ReduceMotion.Never } as const;
