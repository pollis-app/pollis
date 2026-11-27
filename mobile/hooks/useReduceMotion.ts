// The app's one Reduce Motion source (#2125): every animation picks its reduced form from this.
import { useSyncExternalStore } from "react";
import { AccessibilityInfo } from "react-native";
import { useReducedMotion } from "react-native-reanimated";

// The live answer, shared by every caller; null until the OS first replies.
let current: boolean | null = null;
const listeners = new Set<() => void>();
let watching = false;

function set(value: boolean) {
  if (current === value) {
    return;
  }
  current = value;
  for (const listener of listeners) {
    listener();
  }
}

function watch() {
  if (watching) {
    return;
  }
  watching = true;
  AccessibilityInfo.isReduceMotionEnabled().then(set).catch(() => {});
  // App-lifetime: the setting can change while the app runs.
  AccessibilityInfo.addEventListener("reduceMotionChanged", set);
}

function subscribe(listener: () => void) {
  watch();
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** The OS Reduce Motion setting, live. Before the first answer, Reanimated's synchronous launch-time read. */
export function useReduceMotion(): boolean {
  const atLaunch = useReducedMotion();
  return useSyncExternalStore(subscribe, () => current ?? atLaunch);
}
