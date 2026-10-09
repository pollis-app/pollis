// One image page of the media viewer (#1248): pinch to zoom, double-tap to
// zoom in on the tapped point (and again to reset), drag to pan while zoomed.
// The bytes come through `MediaImage`, i.e. the same `useMediaUri` decrypt
// path the chat thumbnail uses — never a second fetch.
//
// The pager owns horizontal swipes, so the pan gesture only exists while the
// image is zoomed; `onZoomChange` tells the pager to stop scrolling then.

import { useCallback, useEffect, useState } from "react";
import { View } from "react-native";
import { Gesture, GestureDetector } from "react-native-gesture-handler";
import Animated, {
  useAnimatedStyle,
  useSharedValue,
  withTiming,
} from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { useTranslation } from "react-i18next";
import { MediaImage } from "../Media";
import type { MessageAttachment } from "../../types";

const MAX_SCALE = 5;
const DOUBLE_TAP_SCALE = 2.5;
const RESET_MS = 180;

export function ZoomableImage({
  attachment,
  width,
  height,
  active,
  onZoomChange,
  testID,
}: {
  attachment: MessageAttachment;
  width: number;
  height: number;
  // Off-screen pages drop back to 1× so the roll never lands on a zoomed one.
  active: boolean;
  onZoomChange: (zoomed: boolean) => void;
  testID?: string;
}) {
  const { t } = useTranslation("mobile");
  const scale = useSharedValue(1);
  const savedScale = useSharedValue(1);
  const tx = useSharedValue(0);
  const ty = useSharedValue(0);
  const savedTx = useSharedValue(0);
  const savedTy = useSharedValue(0);
  const zoomed = useSharedValue(false);
  // Mirrors `zoomed` on the JS side: the pan gesture is rebuilt with it, so
  // at 1× there is no pan to compete with the pager's swipe.
  const [isZoomed, setIsZoomed] = useState(false);
  const handleZoom = useCallback(
    (next: boolean) => {
      setIsZoomed(next);
      onZoomChange(next);
    },
    [onZoomChange],
  );

  useEffect(() => {
    if (!active) {
      scale.value = 1;
      savedScale.value = 1;
      tx.value = 0;
      ty.value = 0;
      savedTx.value = 0;
      savedTy.value = 0;
      zoomed.value = false;
      setIsZoomed(false);
    }
  }, [active, scale, savedScale, tx, ty, savedTx, savedTy, zoomed]);

  // Keep the scaled image covering the page: never pan past its edges.
  const clampX = (value: number, s: number) => {
    "worklet";
    const max = Math.max(0, (width * s - width) / 2);
    return Math.min(max, Math.max(-max, value));
  };
  const clampY = (value: number, s: number) => {
    "worklet";
    const max = Math.max(0, (height * s - height) / 2);
    return Math.min(max, Math.max(-max, value));
  };

  const report = (next: boolean) => {
    "worklet";
    if (zoomed.value !== next) {
      zoomed.value = next;
      scheduleOnRN(handleZoom, next);
    }
  };

  const reset = () => {
    "worklet";
    scale.value = withTiming(1, { duration: RESET_MS });
    tx.value = withTiming(0, { duration: RESET_MS });
    ty.value = withTiming(0, { duration: RESET_MS });
    savedScale.value = 1;
    savedTx.value = 0;
    savedTy.value = 0;
    report(false);
  };

  const pinch = Gesture.Pinch()
    .onUpdate((e) => {
      const next = Math.min(MAX_SCALE, Math.max(1, savedScale.value * e.scale));
      scale.value = next;
      tx.value = clampX(savedTx.value, next);
      ty.value = clampY(savedTy.value, next);
    })
    .onEnd(() => {
      if (scale.value <= 1.02) {
        reset();
        return;
      }
      savedScale.value = scale.value;
      savedTx.value = tx.value;
      savedTy.value = ty.value;
      report(true);
    });

  const pan = Gesture.Pan()
    .enabled(isZoomed)
    .averageTouches(true)
    .onUpdate((e) => {
      if (savedScale.value <= 1) {
        return;
      }
      tx.value = clampX(savedTx.value + e.translationX, scale.value);
      ty.value = clampY(savedTy.value + e.translationY, scale.value);
    })
    .onEnd(() => {
      savedTx.value = tx.value;
      savedTy.value = ty.value;
    });

  const doubleTap = Gesture.Tap()
    .numberOfTaps(2)
    .onEnd((e) => {
      if (savedScale.value > 1) {
        reset();
        return;
      }
      // Zoom towards the tapped point: move it to the page centre.
      const target = DOUBLE_TAP_SCALE;
      const nextX = clampX((width / 2 - e.x) * (target - 1), target);
      const nextY = clampY((height / 2 - e.y) * (target - 1), target);
      scale.value = withTiming(target, { duration: RESET_MS });
      tx.value = withTiming(nextX, { duration: RESET_MS });
      ty.value = withTiming(nextY, { duration: RESET_MS });
      savedScale.value = target;
      savedTx.value = nextX;
      savedTy.value = nextY;
      report(true);
    });

  const gesture = Gesture.Race(doubleTap, Gesture.Simultaneous(pinch, pan));

  const animatedStyle = useAnimatedStyle(() => ({
    transform: [
      { translateX: tx.value },
      { translateY: ty.value },
      { scale: scale.value },
    ],
  }));

  return (
    <GestureDetector gesture={gesture}>
      <View
        testID={testID}
        accessible
        accessibilityRole="image"
        accessibilityLabel={attachment.filename}
        accessibilityHint={t("media.zoomHint")}
        style={{ width, height, overflow: "hidden" }}
      >
        <Animated.View style={[{ width, height }, animatedStyle]}>
          <MediaImage
            attachment={attachment}
            contentFit="contain"
            style={{ width, height, borderRadius: 0, backgroundColor: "transparent" }}
          />
        </Animated.View>
      </View>
    </GestureDetector>
  );
}
