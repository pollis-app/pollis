import { useRef } from "react";
import type { StyleProp, ViewStyle } from "react-native";
import Animated from "react-native-reanimated";
import { CONTENT_FADE_IN, REDUCED_FADE_IN } from "../theme/motion";
import { useReduceMotion } from "../hooks/useReduceMotion";

/**
 * Content that replaces a loading state fades in where it was (#2125); content already there on
 * the first paint (a cached list) shows at once. Renders nothing while `loading`.
 */
export function FadeInAfterLoad({
  loading,
  children,
  style,
}: {
  loading: boolean;
  children: React.ReactNode;
  style?: StyleProp<ViewStyle>;
}) {
  const sawLoading = useRef(loading);
  if (loading) {
    sawLoading.current = true;
  }
  const reduceMotion = useReduceMotion();
  if (loading) {
    return null;
  }
  return (
    <Animated.View
      entering={sawLoading.current ? (reduceMotion ? REDUCED_FADE_IN : CONTENT_FADE_IN) : undefined}
      style={style}
    >
      {children}
    </Animated.View>
  );
}
