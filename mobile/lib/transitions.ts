import type { ComponentProps } from "react";
import { Platform } from "react-native";
import type { Stack } from "expo-router";

// From expo-router's own props, so this file does not import react-navigation.
type StackOptions = Exclude<
  NonNullable<ComponentProps<typeof Stack.Screen>["options"]>,
  (...args: never[]) => unknown
>;

// Stack transitions near 200 ms (#1193). iOS: `simple_push` + `customAnimationOnSwipe` (REQUIRED, or the edge swipe never pops). Android ignores durations: the animation type is the lever.

const DURATION_MS = 200;

export const drillIn: StackOptions = Platform.select({
  ios: {
    animation: "simple_push",
    animationDuration: DURATION_MS,
    customAnimationOnSwipe: true,
  },
  default: { animation: "ios_from_right" },
});

/** Personal settings pages drill in like everything else, so the edge swipe pops them. */
export const settingsPage: StackOptions = drillIn;
