import { useCallback, useMemo, useRef, type ReactNode } from "react";
import { I18nManager, StyleSheet, View } from "react-native";
import { Gesture, GestureDetector } from "react-native-gesture-handler";
import Animated, {
  type SharedValue,
  useAnimatedStyle,
  useSharedValue,
  withSequence,
  withSpring,
  withTiming,
} from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";
import {
  SWIPE_REPLY,
  SWIPE_SPRING,
  arrowProgress,
  dragDistance,
  isArmed,
  panRanges,
  replyDirection,
  towardStart,
} from "../../lib/swipeReply";
import { armHaptic, type HapticEvent } from "../../lib/hapticEvents";
import { haptic } from "../../lib/haptics";

const ARROW = 32;

/**
 * Swipe to reply (#1270): drag a message toward the start edge past the
 * threshold, or double-tap it. Runs on the UI thread; JS is called only when a
 * reply fires or a haptic plays. The pan claims only travel toward the start
 * edge, so vertical scroll and the back gestures are never taken. A drag that
 * takes the touch clears `pressFeedback` (#1282) in the same frame.
 *
 * Without `onReply` the children render as they are, with no gesture.
 */
export function SwipeToReply({
  onReply,
  pressFeedback,
  children,
}: {
  onReply?: () => void;
  pressFeedback?: SharedValue<number>;
  children: ReactNode;
}) {
  // The gestures are built once per row and call the latest handler through
  // this ref, so a list re-render never rebuilds them.
  const onReplyRef = useRef(onReply);
  onReplyRef.current = onReply;
  const fire = useCallback(() => {
    onReplyRef.current?.();
  }, []);
  const buzz = useCallback((event: HapticEvent) => {
    haptic(event);
  }, []);

  const offset = useSharedValue(0);
  const armed = useSharedValue(false);
  const enabled = !!onReply;
  const dir = replyDirection(I18nManager.isRTL);

  const gesture = useMemo(() => {
    const ranges = panRanges(dir);
    const pan = Gesture.Pan()
      .enabled(enabled)
      .activeOffsetX(ranges.activeOffsetX)
      .failOffsetX(ranges.failOffsetX)
      .failOffsetY(ranges.failOffsetY)
      .onStart(() => {
        if (pressFeedback) {
          pressFeedback.value = 0;
        }
      })
      .onUpdate((e) => {
        const travel = towardStart(e.translationX, dir);
        offset.value = dragDistance(travel) * dir;
        const next = isArmed(travel);
        const event = armHaptic(armed.value, next);
        armed.value = next;
        if (event) {
          scheduleOnRN(buzz, event);
        }
      })
      // onEnd, not onFinalize: it runs only for a pan that became active, so a
      // tap (which also begins the pan) never cancels the double tap's nudge.
      .onEnd((_e, success) => {
        if (success && armed.value) {
          scheduleOnRN(fire);
        }
        armed.value = false;
        offset.value = withSpring(0, SWIPE_SPRING);
      });
    const doubleTap = Gesture.Tap()
      .enabled(enabled)
      .numberOfTaps(2)
      .maxDelay(SWIPE_REPLY.doubleTapMs)
      .onEnd((_e, success) => {
        if (!success) {
          return;
        }
        offset.value = withSequence(
          withTiming(SWIPE_REPLY.nudge * dir, { duration: SWIPE_REPLY.nudgeMs }),
          withSpring(0, SWIPE_SPRING),
        );
        scheduleOnRN(buzz, "doubleTapReply");
        scheduleOnRN(fire);
      });
    return Gesture.Race(pan, doubleTap);
  }, [dir, enabled, fire, buzz, offset, armed, pressFeedback]);

  const rowStyle = useAnimatedStyle(() => ({
    transform: [{ translateX: offset.value }],
  }));
  const accent = semantic.accent;
  const raised = semantic.raised;
  const onAccent = semantic.onAccent;
  const arrowStyle = useAnimatedStyle(() => {
    const p = arrowProgress(Math.abs(offset.value));
    return {
      opacity: p,
      transform: [{ scale: 0.5 + p * 0.5 }],
      backgroundColor: p >= 1 ? accent : raised,
    };
  });
  const armedGlyph = useAnimatedStyle(() => ({
    opacity: arrowProgress(Math.abs(offset.value)) >= 1 ? 1 : 0,
  }));

  if (!enabled) {
    return <>{children}</>;
  }

  return (
    <View>
      {/* Not accessible: the row's own "Reply" action is the spoken path. */}
      <View
        pointerEvents="none"
        importantForAccessibility="no-hide-descendants"
        accessibilityElementsHidden
        style={[StyleSheet.absoluteFill, styles.track]}
      >
        <Animated.View style={[styles.arrow, arrowStyle]}>
          <Icon.reply size={18} color={semantic.accent} />
          <Animated.View style={[StyleSheet.absoluteFill, styles.center, armedGlyph]}>
            <Icon.reply size={18} color={onAccent} />
          </Animated.View>
        </Animated.View>
      </View>
      <GestureDetector gesture={gesture}>
        <Animated.View style={rowStyle}>{children}</Animated.View>
      </GestureDetector>
    </View>
  );
}

const styles = StyleSheet.create({
  track: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "flex-end",
    paddingEnd: (SWIPE_REPLY.threshold - ARROW) / 2 + 4,
  },
  arrow: {
    width: ARROW,
    height: ARROW,
    borderRadius: ARROW / 2,
    alignItems: "center",
    justifyContent: "center",
  },
  center: {
    alignItems: "center",
    justifyContent: "center",
  },
});
