// What arrives after its row painted (a pill row, a custom emoji image) fades in within its space (#2115).

import { FadeIn, ReduceMotion } from "react-native-reanimated";

/** 120 ms; `ReduceMotion.System` skips it under Reduce Motion. */
export const LATE_FADE_IN = FadeIn.duration(120).reduceMotion(ReduceMotion.System);
