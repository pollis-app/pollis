import { useCallback, useEffect, useRef, useState } from "react";
import {
  Modal,
  Text,
  View,
  AccessibilityInfo,
  Platform,
  useWindowDimensions,
} from "react-native";
import Animated, {
  Easing,
  cancelAnimation,
  useAnimatedStyle,
  useReducedMotion,
  useSharedValue,
  withTiming,
} from "react-native-reanimated";
import { scheduleOnRN } from "react-native-worklets";
import { useTranslation } from "react-i18next";
import { KeyboardAvoidingView } from "react-native-keyboard-controller";
import { semantic, type as ty, r, space, layout } from "../../theme/tokens";
import { useTheme } from "../theme";
import { Icon } from "../icons";
import { useBottomInset, PressInPressable, InSheetContext } from "../ui";
import { useIsRegular } from "../../hooks/useLayoutClass";

// Entry timing (#1193), in line with the ~200 ms stack transitions.
const ENTER_MS = 180;
const SETTLE_FALLBACK_MS = 600;

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

/** Bottom sheet in a transparent `Modal` (full-screen backdrop, focus confined). Dismissal is immediate. */
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
  const regular = useIsRegular();
  // Start below the screen edge: no measurement needed (#1249).
  const { height: windowHeight } = useWindowDimensions();
  const offscreen = useRef(windowHeight).current;
  // 0 → 1 over the entrance, on the UI thread.
  const progress = useSharedValue(0);
  // A ref: read only when the entrance would start.
  const reduceMotionAtLaunch = useReducedMotion();
  const reduceMotion = useRef(reduceMotionAtLaunch);
  // Once settled, plain static styles: visibility no longer depends on Reanimated.
  const [settled, setSettled] = useState(reduceMotionAtLaunch);
  const markSettled = useCallback(() => setSettled(true), []);

  useEffect(() => {
    AccessibilityInfo.isReduceMotionEnabled()
      .then((enabled) => {
        reduceMotion.current = enabled;
      })
      .catch(() => {});
  }, []);

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
      { duration: ENTER_MS, easing: Easing.out(Easing.cubic) },
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
    if (!settled) {
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
      onRequestClose={onClose}
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
              settled ? SETTLED_BACKDROP : null,
            ]}
          >
            {/* Hidden from screen readers: Close is the accessible way out. */}
            <PressInPressable
              pressIn
              onPress={onClose}
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
            style={{ flex: 1, justifyContent: "flex-end" }}
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
                },
                cardStyle,
                settled ? SETTLED_CARD : null,
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
                  onPress={onClose}
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
