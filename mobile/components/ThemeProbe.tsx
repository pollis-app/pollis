import { View } from "react-native";
import { bootAccentHex, currentAccentHex } from "../theme/tokens";

// Full-size, not 1×1: Android's UI hierarchy leaves out a tiny empty view, so
// Maestro could not see it. <Screen> draws these first, under its content, and
// they take no touches, so nothing above them notices.
const PROBE = { position: "absolute", top: 0, bottom: 0, start: 0, end: 0 } as const;

// Invisible e2e hooks (#1447), drawn by <Screen> so they sit on every page:
//   theme-accent-<rrggbb>       the accent this screen rendered with, read from
//                               the live tokens like every style on the page;
//   theme-boot-accent-<rrggbb>  the accent the tokens were seeded with when the
//                               app loaded, i.e. what the first frame painted.
// No colour, no content to speak, no touches.
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
    </>
  );
}
