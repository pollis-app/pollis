import { useCallback, useEffect, useRef, useState } from "react";
import {
  Modal,
  Pressable,
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
import { useBottomInset } from "../ui";
import { useIsRegular } from "../../hooks/useLayoutClass";

const ENTER_MS = 180;
// Upper bound on the entrance: past this the sheet is shown statically.
const SETTLE_FALLBACK_MS = 600;

// The end state, applied as plain styles AFTER the animated ones once the
// entrance settles, in case Reanimated's UI-thread update was dropped. The
// animated styles stay attached: detaching one does not revert what it wrote.
const SETTLED_BACKDROP = { opacity: 1 };
const SETTLED_CARD = { transform: [{ translateY: 0 }] };

/**
 * Runs `fn` once a sheet that was just closed (unmounted in the same handler)
 * is really gone; use it for a navigation or another sheet that follows.
 *
 * iOS needs it: a sheet is a presented Modal, and a push or second Modal in
 * the same tick races the dismissal — UIKit can drop the dismiss or refuse the
 * new presentation, so the next sheet never appears. Two frames covers the
 * commit plus the dismissal. Android runs `fn` immediately.
 */
export function afterSheetClose(fn: () => void): void {
  if (Platform.OS !== "ios") {
    fn();
    return;
  }
  requestAnimationFrame(() => requestAnimationFrame(fn));
}

/**
 * Bottom sheet in a transparent `Modal`, so the backdrop covers the whole
 * screen and screen-reader focus is confined to the sheet. On regular width
 * the card is at most `layout.sheetMaxWidth` wide and centred. No drag handle:
 * a 44×44 Close button, a backdrop tap or Android back closes it.
 *
 * Every sheet should pass `title` (the dialog's spoken name). Dismissal is
 * immediate, since a sheet's actions usually navigate away.
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
  // Anchor on the sheet card for e2e flows.
  testID?: string;
  // The Close button's id; sheets whose flows tap an older dismiss id pass it.
  closeTestID?: string;
}) {
  useTheme();
  const { t } = useTranslation("common");
  // The root SafeAreaProvider's insets are right here because the Modal is
  // status- and navigation-bar translucent, so its window is full-screen like
  // the activity's. (A nested provider would delay the sheet until it measured.)
  const bottomInset = useBottomInset();
  const regular = useIsRegular();
  // The window height is below the screen edge whatever the card's size, so
  // the slide needs no measurement (#1249). Read once.
  const { height: windowHeight } = useWindowDimensions();
  const offscreen = useRef(windowHeight).current;
  // 0 → 1 over the entrance, on the UI thread; backdrop opacity and card
  // translateY both derive from it, so no frame waits on the JS thread.
  const progress = useSharedValue(0);
  // Reanimated reads the system setting at app start; the AccessibilityInfo
  // query below catches a change made since. A ref: it must not re-render.
  const reduceMotionAtLaunch = useReducedMotion();
  const reduceMotion = useRef(reduceMotionAtLaunch);
  // Once settled, render plain static styles so visibility no longer depends
  // on Reanimated. Reduced motion starts settled.
  const [settled, setSettled] = useState(reduceMotionAtLaunch);
  const markSettled = useCallback(() => setSettled(true), []);

  useEffect(() => {
    AccessibilityInfo.isReduceMotionEnabled()
      .then((enabled) => {
        reduceMotion.current = enabled;
      })
      .catch(() => {});
  }, []);

  // On iOS the content mounts before the modal controller is presented, and an
  // animation started then can be lost, leaving the sheet invisible. So the
  // entrance starts from onShow, with no layout measurement.
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

  // Whatever happens to onShow or the animation, the sheet is fully visible
  // shortly after it mounts.
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_FALLBACK_MS);
    return () => clearTimeout(timer);
  }, []);

  // Pin the shared value to its end too: a stalled timing (Reanimated 4.4+,
  // gorhom #2721) would hold the props it last wrote, which override the
  // static SETTLED_* styles.
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
          <Pressable
            onPress={onClose}
            accessible={false}
            importantForAccessibility="no"
            style={{ flex: 1 }}
          />
        </Animated.View>
        {/* A sheet with a field rides above the keyboard the same way <Screen>
            does (#1246). Taps outside the card fall through to the backdrop. */}
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
                // Opaque: a translucent card let the header and composer show
                // through its buttons (#1193).
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
              <Pressable
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
              </Pressable>
            </View>
            {children}
          </Animated.View>
        </KeyboardAvoidingView>
      </View>
    </Modal>
  );
}
