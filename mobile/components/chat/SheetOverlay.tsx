import { useCallback, useEffect, useRef, useState } from "react";
import {
  Modal,
  View,
  Platform,
  useWindowDimensions,
} from "react-native";
import { Text } from "../Text";
import Animated, {
  ReduceMotion,
  cancelAnimation,
  useAnimatedStyle,
  useSharedValue,
  withTiming,
} from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { useTranslation } from "react-i18next";
import { KeyboardAvoidingView } from "react-native-keyboard-controller";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { semantic, type as ty, r, space, layout } from "../../theme/tokens";
import { useTheme } from "../theme";
import { Icon } from "../icons";
import { useBottomInset, PressInPressable, InSheetContext } from "../ui";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { useReduceMotion } from "../../hooks/useReduceMotion";
import { EASE_IN, EASE_OUT, MOTION } from "../../theme/motion";

// Entry timing (#1193), in line with the ~200 ms stack transitions; the shared tokens (#2125).
const ENTER_MS = MOTION.baseMs;
const SETTLE_FALLBACK_MS = 600;
// The close plays the entrance backwards (#2125). `onClose` runs from a JS timer at EXIT_MS, never
// from the animation's callback, so a stalled animation can never hold a sheet open.
const EXIT_MS = MOTION.exitMs;

// Applied AFTER the animated styles once settled, in case Reanimated never wrote the view.
const SETTLED_BACKDROP = { opacity: 1 };
const SETTLED_CARD = { transform: [{ translateY: 0 }] };

/** Run `fn` once a just-closed sheet is gone: iOS drops a push or Modal raced against the dismissal. */
export function afterSheetClose(fn: () => void): void {
  if (Platform.OS !== "ios") {
    fn();
    return;
  }
  requestAnimationFrame(() => requestAnimationFrame(fn));
}

/** Bottom sheet in a transparent `Modal` (full-screen backdrop, focus confined). Its own dismissals (Close, backdrop, Back) play the entrance backwards. */
export function SheetOverlay({
  onClose,
  title,
  children,
  testID,
  closeTestID = "btn-sheet-close",
}: {
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  testID?: string;
  closeTestID?: string;
}) {
  useTheme();
  const { t } = useTranslation("common");
  // Root insets are right here: the Modal is translucent under both system bars.
  const bottomInset = useBottomInset();
  const topInset = useSafeAreaInsets().top;
  const regular = useIsRegular();
  // Start below the screen edge: no measurement needed (#1249).
  const { height: windowHeight } = useWindowDimensions();
  const offscreen = useRef(windowHeight).current;
  // 0 → 1 over the entrance, on the UI thread.
  const progress = useSharedValue(0);
  // A ref: read only when the entrance or the exit would start.
  const reduceMotionNow = useReduceMotion();
  const reduceMotion = useRef(reduceMotionNow);
  reduceMotion.current = reduceMotionNow;
  // Once settled, plain static styles: visibility no longer depends on Reanimated.
  const [settled, setSettled] = useState(reduceMotionNow);
  const markSettled = useCallback(() => setSettled(true), []);
  // Closing: the settled styles step aside so the exit shows; `onClose` fires once, from a timer.
  const [closing, setClosing] = useState(false);
  const closingRef = useRef(false);
  const closedRef = useRef(false);
  const closeTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const onCloseRef = useRef(onClose);
  onCloseRef.current = onClose;
  const finishClose = useCallback(() => {
    if (closedRef.current) {
      return;
    }
    closedRef.current = true;
    onCloseRef.current();
  }, []);
  const requestClose = useCallback(() => {
    if (closingRef.current || closedRef.current) {
      return;
    }
    closingRef.current = true;
    if (reduceMotion.current) {
      finishClose();
      return;
    }
    setClosing(true);
    cancelAnimation(progress);
    progress.value = withTiming(0, {
      duration: EXIT_MS,
      easing: EASE_IN,
      reduceMotion: ReduceMotion.Never,
    });
    closeTimer.current = setTimeout(finishClose, EXIT_MS);
  }, [finishClose, progress]);
  // Unmounted by its parent (a row's action closed it): nothing left to close.
  useEffect(
    () => () => {
      closedRef.current = true;
      if (closeTimer.current) {
        clearTimeout(closeTimer.current);
      }
    },
    [],
  );

  // iOS can lose an animation started before the modal is presented (review #1): start on onShow.
  const onShow = () => {
    if (settled) {
      return;
    }
    if (reduceMotion.current) {
      setSettled(true);
      return;
    }
    progress.value = withTiming(
      1,
      { duration: ENTER_MS, easing: EASE_OUT, reduceMotion: ReduceMotion.Never },
      (finished) => {
        "worklet";
        if (finished) {
          scheduleOnRN(markSettled);
        }
      },
    );
  };

  // Belt and braces: fully visible shortly after mount.
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_FALLBACK_MS);
    return () => clearTimeout(timer);
  }, []);

  // A stalled timing (gorhom #2721) would hold props that override the SETTLED_* styles.
  useEffect(() => {
    // Never snap a closing sheet back open.
    if (!settled || closingRef.current) {
      return;
    }
    cancelAnimation(progress);
    progress.value = 1;
  }, [settled, progress]);

  const backdropStyle = useAnimatedStyle(() => ({ opacity: progress.value }));
  const cardStyle = useAnimatedStyle(() => ({
    transform: [{ translateY: (1 - progress.value) * offscreen }],
  }));

  return (
    <Modal
      visible
      transparent
      animationType="none"
      statusBarTranslucent
      navigationBarTranslucent
      onRequestClose={requestClose}
      onShow={onShow}
    >
      {/* Android keeps every control in here on release (lib/pressIn.ts). */}
      <InSheetContext.Provider value={true}>
        <View style={{ flex: 1 }}>
          <Animated.View
            style={[
              {
                position: "absolute",
                top: 0,
                bottom: 0,
                start: 0,
                end: 0,
                backgroundColor: semantic.backdrop,
              },
              backdropStyle,
              settled && !closing ? SETTLED_BACKDROP : null,
            ]}
          >
            {/* Hidden from screen readers: Close is the accessible way out. */}
            <PressInPressable
              pressIn
              onPress={requestClose}
              accessible={false}
              importantForAccessibility="no"
              style={{ flex: 1 }}
            />
          </Animated.View>
          {/* Rides above the keyboard, like Screen (#1246). */}
          <KeyboardAvoidingView
            pointerEvents="box-none"
            behavior="padding"
            automaticOffset
            // #2123: a sheet taller than the room above the keyboard stops below the status bar.
            style={{ flex: 1, justifyContent: "flex-end", paddingTop: topInset + space.lg }}
          >
            <Animated.View
              testID={testID}
              accessibilityViewIsModal
              style={[
                {
                  // Opaque (#1193).
                  backgroundColor: semantic.sheetBg,
                  width: "100%",
                  maxWidth: regular ? layout.sheetMaxWidth : undefined,
                  alignSelf: "center",
                  borderTopStartRadius: r.sheet,
                  borderTopEndRadius: r.sheet,
                  borderTopWidth: 1,
                  borderTopColor: semantic.hair,
                  paddingHorizontal: space.xxl,
                  paddingTop: space.xl,
                  paddingBottom: bottomInset + space.lg,
                  gap: space.lg,
                  // Shrinks into that room; a child that can give (a list) does.
                  flexShrink: 1,
                  minHeight: 0,
                },
                cardStyle,
                settled && !closing ? SETTLED_CARD : null,
              ]}
            >
              <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
                <Text
                  accessibilityRole="header"
                  numberOfLines={2}
                  style={[ty.heading, { flex: 1, color: semantic.text }]}
                >
                  {title ?? ""}
                </Text>
                <PressInPressable
                  pressIn
                  onPress={requestClose}
                  testID={closeTestID}
                  accessibilityRole="button"
                  accessibilityLabel={t("actions.close")}
                  hitSlop={4}
                  style={{
                    width: 44,
                    height: 44,
                    borderRadius: 22,
                    alignItems: "center",
                    justifyContent: "center",
                    backgroundColor: semantic.high,
                  }}
                >
                  <Icon.close size={20} color={semantic.text} />
                </PressInPressable>
              </View>
              {children}
            </Animated.View>
          </KeyboardAvoidingView>
        </View>
      </InSheetContext.Provider>
    </Modal>
  );
}
