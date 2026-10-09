import { View } from "react-native";
import Svg, { Rect } from "react-native-svg";
import { semantic } from "../../theme/tokens";

// A deterministic scatter: the same field on every launch.
const COUNT = 90;
const W = 390;
const H = 844;

/**
 * The Initializing screen's quiet backdrop: a sparse, static field of small
 * accent squares behind the card. Decorative only — hidden from screen
 * readers, never animated.
 */
export function DotField() {
  return (
    <View
      pointerEvents="none"
      accessibilityElementsHidden
      importantForAccessibility="no-hide-descendants"
      style={{ position: "absolute", top: 0, start: 0, end: 0, bottom: 0, opacity: 0.3 }}
    >
      <Svg width="100%" height="100%" viewBox={`0 0 ${W} ${H}`} preserveAspectRatio="xMidYMid slice">
        {Array.from({ length: COUNT }).map((_, i) => {
          const x = (i * 74) % W;
          const y = (i * 107) % H;
          const size = (i * 7) % 5 < 2 ? 3 : 2;
          return (
            <Rect
              key={i}
              x={x}
              y={y}
              width={size}
              height={size}
              fill={semantic.accent}
              opacity={((i % 9) + 3) / 24}
            />
          );
        })}
      </Svg>
    </View>
  );
}
