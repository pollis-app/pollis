import type { ComponentProps } from "react";
import { Platform } from "react-native";
import type { Stack } from "expo-router";

// The object form of a stack screen's `options`, taken from expo-router's own
// props so this file does not import react-navigation (a transitive dep).
type StackOptions = Exclude<
  NonNullable<ComponentProps<typeof Stack.Screen>["options"]>,
  (...args: never[]) => unknown
>;

// Stack transition timing (#1193). The platform defaults (~350 ms on iOS,
// 400 ms on Android) feel sluggish in a messenger, where moving between
// conversations is the core loop; Slack and Discord sit near 200 ms.
//
// The two platforms need different levers:
//
// - iOS honours `animationDuration`, but only for `simple_push`,
//   `slide_from_bottom`, `fade` and `fade_from_bottom`, never for the native
//   `slide_from_right` push. So drill-in uses `simple_push`: the same
//   horizontal push minus the header cross-fade we never show anyway
//   (`headerShown: false`). The edge swipe-back stays the native interactive
//   pop because `customAnimationOnSwipe` is left off.
// - Android ignores `animationDuration` entirely; each animation type has a
//   fixed native duration. `slide_from_right` / `slide_from_bottom` run at the
//   system medium time (400 ms); `ios_from_right` runs at the short time
//   (200 ms) and `fade` at 150 ms, so the lever there is the animation choice.

const DURATION_MS = 200;

/** Drill-in: tab → group → channel → thread, and back. */
export const drillIn: StackOptions = Platform.select({
  ios: { animation: "simple_push", animationDuration: DURATION_MS },
  default: { animation: "ios_from_right" },
});

/**
 * Personal settings pages. iOS keeps the slide up from the bottom, shortened;
 * Android has no bottom slide under 350 ms, so it fades instead.
 */
export const settingsPage: StackOptions = Platform.select({
  ios: { animation: "slide_from_bottom", animationDuration: DURATION_MS },
  default: { animation: "fade" },
});
