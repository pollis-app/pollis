import { useEffect, useState } from "react";
import { Keyboard, Platform } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";

/**
 * How far the soft keyboard reaches into the screen on Android, above the
 * bottom safe-area inset the screen already pads for. Always 0 on iOS, where
 * `KeyboardAvoidingView` does the job.
 *
 * Android needs its own path because the app is edge-to-edge: the window no
 * longer shrinks for the keyboard (`adjustResize` is inert), and React Native's
 * `KeyboardAvoidingView` only reacts to keyboard SHOW events. Navigating from
 * one focused field to the next screen's autofocused field (email → OTP) keeps
 * the keyboard up, so the new screen never hears a show event and its bottom
 * action stays buried under the keyboard. Seeding from `Keyboard.metrics()` at
 * mount covers the keyboard that was already open.
 */
export function useAndroidKeyboardInset(): number {
  const insets = useSafeAreaInsets();
  const [height, setHeight] = useState(() =>
    Platform.OS === "android" ? (Keyboard.metrics()?.height ?? 0) : 0,
  );

  useEffect(() => {
    if (Platform.OS !== "android") {
      return;
    }
    const shown = Keyboard.addListener("keyboardDidShow", (e) => {
      setHeight(e.endCoordinates.height);
    });
    const hidden = Keyboard.addListener("keyboardDidHide", () => {
      setHeight(0);
    });
    return () => {
      shown.remove();
      hidden.remove();
    };
  }, []);

  return Math.max(0, height - insets.bottom);
}
