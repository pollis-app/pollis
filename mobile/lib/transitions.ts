import type { ComponentProps } from "react";
import { Platform } from "react-native";
import type { Stack } from "expo-router";
import { MOTION } from "../../design/theme.ts";

// From expo-router's own props, so this file does not import react-navigation.
type StackOptions = Exclude<
  NonNullable<ComponentProps<typeof Stack.Screen>["options"]>,
  (...args: never[]) => unknown
>;

// Stack transitions near 200 ms (#1193). iOS: `simple_push` + `customAnimationOnSwipe` (REQUIRED, or the edge swipe never pops). Android ignores durations: the animation type is the lever.

const DURATION_MS = 200;

// Every push uses it, personal settings pages included, so the edge swipe pops them.
const DRILL_IN: StackOptions = Platform.select({
  ios: {
    animation: "simple_push",
    animationDuration: DURATION_MS,
    customAnimationOnSwipe: true,
  },
  default: { animation: "ios_from_right" },
});

// Reduce Motion (#2125): a push cross-fades instead of sliding. The edge swipe still pops, its
// screen following the finger (the system's own interactive pop).
const DRILL_IN_REDUCED: StackOptions = {
  animation: "fade",
  animationDuration: MOTION.baseMs,
};

/** The push every stack screen uses; pass `useReduceMotion()`. */
export function drillIn(reduceMotion: boolean): StackOptions {
  return reduceMotion ? DRILL_IN_REDUCED : DRILL_IN;
}
