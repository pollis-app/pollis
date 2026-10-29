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

// In line with the ~200 ms stack transitions (#1193).
const ENTER_MS = 180;
const SETTLE_FALLBACK_MS = 600;

// Applied AFTER the animated styles once settled, so a view Reanimated never
// wrote still shows them. The animated styles stay attached: detaching one
// does not revert what it already wrote.
const SETTLED_BACKDROP = { opacity: 1 };
const SETTLED_CARD = { transform: [{ translateY: 0 }] };

/**
 * Runs `fn` once a just-closed sheet is really gone; use it for a navigation
 * or a second sheet started from a sheet action.
 *
 * On iOS a Modal is a presented view controller, dismissed only when Fabric
 * mounts the commit: a push or Modal in the same tick can drop the dismiss or
 * be refused ("already presenting"). Two frames cover it. Android runs `fn` now.
 */
export function afterSheetClose(fn: () => void): void {
  if (Platform.OS !== "ios") {
    fn();
    return;
  }
  requestAnimationFrame(() => requestAnimationFrame(fn));
}

/**
 * The one bottom sheet. A transparent `Modal`, so the backdrop covers the
 * whole screen and screen-reader focus stays in the sheet; centred and at
 * most `layout.sheetMaxWidth` on iPad. Every sheet should pass `title` (the
 * dialog's spoken name). Dismissal is immediate: a sheet's actions usually
 * navigate and must not wait on an exit animation.
 */
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
  // For flows that tap an older dismiss id.
  closeTestID?: string;
}) {
  useTheme();
  const { t } = useTranslation("common");
  // The root SafeAreaProvider's insets are right here because the Modal is
  // status- and navigation-bar translucent, full-screen like the activity.
  const bottomInset = useBottomInset();
  const regular = useIsRegular();
  // The window height is off-screen whatever the card's size, so the slide
  // needs no measurement (#1249). Read once.
  const { height: windowHeight } = useWindowDimensions();
  const offscreen = useRef(windowHeight).current;
  // Backdrop and card both derive from it on the UI thread.
  const progress = useSharedValue(0);
  // Reanimated reads the setting at launch; the query below catches a later
  // change. A ref: read only when the entrance starts.
  const reduceMotionAtLaunch = useReducedMotion();
  const reduceMotion = useRef(reduceMotionAtLaunch);
  // Once settled, plain static styles: visibility no longer depends on
  // Reanimated. Reduced motion starts settled.
  const [settled, setSettled] = useState(reduceMotionAtLaunch);
  const markSettled = useCallback(() => setSettled(true), []);

  useEffect(() => {
    AccessibilityInfo.isReduceMotionEnabled()
      .then((enabled) => {
        reduceMotion.current = enabled;
      })
      .catch(() => {});
  }, []);

  // On iOS an animation started before the modal controller is presented can
  // be lost, leaving the sheet present but invisible, so it starts from onShow.
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

  // Whatever happens to onShow or the animation, the sheet shows shortly.
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_FALLBACK_MS);
    return () => clearTimeout(timer);
  }, []);

  // Pin the value to its end too: a timing Reanimated 4.4+ stalled part-way
  // (gorhom #2721) would hold props that override the static SETTLED_* styles.
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
      {/* On Android everything in here acts on release: see pressInSupported. */}
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
            {/* Hidden from screen readers: the Close button is the accessible way out. */}
            <PressInPressable
              pressIn
              onPress={onClose}
              accessible={false}
              importantForAccessibility="no"
              style={{ flex: 1 }}
            />
          </Animated.View>
          {/* A sheet with a field rides above the keyboard (#1246); taps outside the
              card fall through to the backdrop. */}
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
                  // Opaque: a translucent card let the screen show through its buttons (#1193).
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
