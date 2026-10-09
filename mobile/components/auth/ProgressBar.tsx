import { useEffect, useRef, useState } from "react";
import { Animated, Easing, View } from "react-native";
import { semantic } from "../../theme/tokens";
import { useReduceMotion } from "./useReduceMotion";

const HEIGHT = 6;
const SWEEP_MS = 1400;

/**
 * A thin rounded progress track. `percent` is the static fill; while
 * `indeterminate`, a short segment sweeps along the track instead — unless
 * Reduce Motion is on, when the static fill stays put.
 */
export function ProgressBar({
  percent,
  indeterminate,
  label,
}: {
  percent: number;
  indeterminate?: boolean;
  label: string;
}) {
  const reduceMotion = useReduceMotion();
  const [width, setWidth] = useState(0);
  const sweep = useRef(new Animated.Value(0)).current;
  const animate = !!indeterminate && !reduceMotion && width > 0;

  useEffect(() => {
    if (!animate) {
      sweep.stopAnimation();
      return;
    }
    sweep.setValue(0);
    const loop = Animated.loop(
      Animated.timing(sweep, {
        toValue: 1,
        duration: SWEEP_MS,
        easing: Easing.inOut(Easing.quad),
        useNativeDriver: true,
      }),
    );
    loop.start();
    return () => loop.stop();
  }, [animate, sweep]);

  const segment = width * 0.35;
  return (
    <View
      accessible
      accessibilityRole="progressbar"
      accessibilityLabel={label}
      accessibilityValue={{ min: 0, max: 100, now: percent }}
      onLayout={(e) => setWidth(e.nativeEvent.layout.width)}
      style={{
        height: HEIGHT,
        borderRadius: HEIGHT / 2,
        backgroundColor: semantic.high,
        overflow: "hidden",
      }}
    >
      {animate ? (
        <Animated.View
          style={{
            height: HEIGHT,
            width: segment,
            borderRadius: HEIGHT / 2,
            backgroundColor: semantic.accent,
            transform: [
              {
                translateX: sweep.interpolate({
                  inputRange: [0, 1],
                  outputRange: [-segment, width],
                }),
              },
            ],
          }}
        />
      ) : (
        <View
          style={{
            height: HEIGHT,
            width: `${percent}%`,
            borderRadius: HEIGHT / 2,
            backgroundColor: semantic.accent,
          }}
        />
      )}
    </View>
  );
}
