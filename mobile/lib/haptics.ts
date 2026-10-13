import { Platform } from "react-native";
import * as Haptics from "expo-haptics";
import { HAPTICS, type HapticEvent } from "./hapticEvents";

const ANDROID: Record<string, Haptics.AndroidHaptics> = {
  "context-click": Haptics.AndroidHaptics.Context_Click,
  "clock-tick": Haptics.AndroidHaptics.Clock_Tick,
  "long-press": Haptics.AndroidHaptics.Long_Press,
  "keyboard-tap": Haptics.AndroidHaptics.Keyboard_Tap,
  "confirm": Haptics.AndroidHaptics.Confirm,
  "reject": Haptics.AndroidHaptics.Reject,
};

const IMPACT = {
  light: Haptics.ImpactFeedbackStyle.Light,
  medium: Haptics.ImpactFeedbackStyle.Medium,
  heavy: Haptics.ImpactFeedbackStyle.Heavy,
};

const NOTIFICATION = {
  success: Haptics.NotificationFeedbackType.Success,
  warning: Haptics.NotificationFeedbackType.Warning,
  error: Haptics.NotificationFeedbackType.Error,
};

/**
 * Plays the tactile feedback for `event` (#1286; the table is in
 * lib/hapticEvents.ts). Fire-and-forget: feedback is a nicety, so a device
 * without a haptic engine, or any failure, is silent and never blocks the
 * interaction that triggered it.
 */
export function haptic(event: HapticEvent): void {
  const spec = HAPTICS[event];
  let p: Promise<void> | undefined;
  try {
    if (Platform.OS === "android") {
      p = Haptics.performAndroidHapticsAsync(ANDROID[spec.android]);
    } else if (Platform.OS === "ios") {
      const ios = spec.ios;
      if (ios.kind === "selection") {
        p = Haptics.selectionAsync();
      } else if (ios.kind === "impact") {
        p = Haptics.impactAsync(IMPACT[ios.style]);
      } else {
        p = Haptics.notificationAsync(NOTIFICATION[ios.type]);
      }
    }
  } catch {
    return;
  }
  p?.catch(() => {});
}
