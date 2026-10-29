import type { ComponentProps } from "react";
import { Platform } from "react-native";
import type { Stack } from "expo-router";

// From expo-router's own props, so this file does not import react-navigation.
type StackOptions = Exclude<
  NonNullable<ComponentProps<typeof Stack.Screen>["options"]>,
  (...args: never[]) => unknown
>;

// Stack transitions at ~200 ms (#1193); platform defaults (350-400 ms) feel
// sluggish in a messenger.
//
// - iOS honours `animationDuration` only for `simple_push` and a few others,
//   never the native push, so drill-in uses `simple_push`.
//   `customAnimationOnSwipe` is REQUIRED with it: without it the edge swipe
//   begins but never pops (react-native-screens hands the pop a
//   non-interactive animator).
// - Android ignores `animationDuration`; the lever is the animation choice
//   (`ios_from_right` runs at the 200 ms short time).

const DURATION_MS = 200;

export const drillIn: StackOptions = Platform.select({
  ios: {
    animation: "simple_push",
    animationDuration: DURATION_MS,
    customAnimationOnSwipe: true,
  },
  default: { animation: "ios_from_right" },
});

/** Every pushed screen's back is at the top and the edge swipe must pop it. */
export const settingsPage: StackOptions = drillIn;
