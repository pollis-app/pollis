import { View } from "react-native";
import { useKeyboardState } from "react-native-keyboard-controller";

// Full-size: Android's hierarchy omits a tiny empty view, so Maestro could not see it.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hook (#1641): `keyboard-open` / `keyboard-closed` is what keyboard-controller last told JS, the state every KeyboardAvoidingView pads from. A hide it never heard about reads `keyboard-open` with no keyboard on screen.
export function KeyboardProbe() {
  const visible = useKeyboardState((s) => s.isVisible);
  return (
    <View
      testID={visible ? "keyboard-open" : "keyboard-closed"}
      collapsable={false}
      pointerEvents="none"
      style={PROBE}
    />
  );
}
