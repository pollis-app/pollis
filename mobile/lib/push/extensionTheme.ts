// The live accent and background, for the iOS Notification Service Extension's letter avatars
// (#1599). Written to the App Group's UserDefaults by the native `PollisPushState` module
// (modules/pollis-native/ios/PollisPushState.mm) and read in plugins/notification-service/
// LetterAvatar.swift, which falls back to the default theme. Android has its own
// `PollisPushState.setThemeColors`, driven from lib/push/openConversation.ts.

import { NativeModules, Platform } from "react-native";

type ThemeModule = { setThemeColors(accentHex: string, bgHex: string): void };

/** Best-effort: a binary without the module (or Android) does nothing. */
export function saveThemeForExtension(accentHex: string, backgroundHex: string): void {
  if (Platform.OS !== "ios") {
    return;
  }
  const native = (NativeModules as { PollisPushState?: ThemeModule }).PollisPushState;
  try {
    native?.setThemeColors(accentHex, backgroundHex);
  } catch {
    // The extension keeps the colours it had, or the defaults.
  }
}
