import { View } from "react-native";
import { bootAccentHex, currentAccentHex, currentBackgroundHex } from "../theme/tokens";

// Full-size: Android's hierarchy omits a tiny empty view, so Maestro could not see it.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hooks (#1447): `theme-accent-<rrggbb>` (live), `theme-boot-accent-<rrggbb>` (first frame)
// and `theme-bg-<rrggbb>` (the live background, #2107).
export function ThemeProbe() {
  return (
    <>
      <View
        testID={`theme-accent-${currentAccentHex().slice(1)}`}
        collapsable={false}
        pointerEvents="none"
        style={PROBE}
      />
      <View
        testID={`theme-boot-accent-${bootAccentHex().slice(1)}`}
        collapsable={false}
        pointerEvents="none"
        style={PROBE}
      />
      <View
        testID={`theme-bg-${currentBackgroundHex().slice(1)}`}
        collapsable={false}
        pointerEvents="none"
        style={PROBE}
      />
    </>
  );
}
