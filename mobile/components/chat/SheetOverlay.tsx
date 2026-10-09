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

// Entry timing (#1193): in line with the ~200 ms stack transitions — fast
// enough to read as a response to the tap, not a presentation.
const ENTER_MS = 180;
// Upper bound on the entrance: past this the sheet is shown statically.
const SETTLE_FALLBACK_MS = 600;

// The end state, applied as plain React styles AFTER the animated ones once
// the entrance settles. If Reanimated never wrote the view (its UI-thread
// update was dropped), these are what the view shows. The animated styles
// stay attached, because detaching one does not revert what it already wrote.
const SETTLED_BACKDROP = { opacity: 1 };
const SETTLED_CARD = { transform: [{ translateY: 0 }] };

/**
 * Runs `fn` once a sheet that was just closed (its SheetOverlay unmounted in
 * the same handler) is really gone. Use it for whatever a sheet action does
 * NEXT that presents or navigates: a push / replace, or opening another sheet.
 *
 * iOS needs it: a sheet is a Modal, i.e. a presented view controller, and the
 * unmount only dismisses it when Fabric mounts the commit on the UI thread.
 * A navigation push or a second Modal started in the same tick races that
 * dismissal — UIKit can drop the dismiss (the screen keeps a stale presented
 * controller) or refuse the new presentation ("already presenting"), so the
 * next sheet silently never appears. Two frames covers the commit plus the
 * non-animated dismissal. Android has no presenting controller, so it runs
 * `fn` immediately and behaves exactly as before.
 */
export function afterSheetClose(fn: () => void): void {
  if (Platform.OS !== "ios") {
    fn();
    return;
  }
  requestAnimationFrame(() => requestAnimationFrame(fn));
}

/**
 * Bottom sheet (Actions.dc.html). Rendered in a transparent `Modal`, so the
 * backdrop covers the WHOLE screen — status bar and home-indicator area
 * included — and screen-reader focus is confined to the sheet. The card is
 * anchored to the bottom edge, edge to edge, with a 20pt top radius and the
 * bottom safe-area inset as extra padding. On regular width (iPad) the card
 * is at most `layout.sheetMaxWidth` wide and centred (review6 #4); the
 * backdrop still covers the whole screen. No drag handle: the header row
 * carries the title and a 44×44 Close button; a backdrop tap or Android back
 * also closes.
 *
 * `title` is optional only so unmigrated sheets compile; every sheet should
 * pass one (it is the dialog's spoken name). Dismissal is immediate, since a
 * sheet's actions usually navigate away and must not wait on an exit
 * animation.
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
  // The root SafeAreaProvider's insets reach the Modal through React context.
  // They are the right ones here because the Modal is statusBarTranslucent +
  // navigationBarTranslucent: its window is full-screen under both system
  // bars, exactly like the activity's. (A nested SafeAreaProvider would
  // measure the same values but render nothing until its first native
  // measurement, delaying the sheet.) Android gets the nav-bar floor.
  const bottomInset = useBottomInset();
  const regular = useIsRegular();
  // Off-screen start for the card: the window height is below the screen
  // edge whatever the card's size, so the slide needs no measurement and can
  // start the moment the Modal is shown (#1249). Read once: a rotation
  // mid-entrance only changes where an already-running slide began.
  const { height: windowHeight } = useWindowDimensions();
  const offscreen = useRef(windowHeight).current;
  // 0 → 1 over the entrance, on the UI thread (Reanimated). Backdrop opacity
  // and card translateY both derive from it in useAnimatedStyle, so once the
  // timing starts no frame waits on the JS thread.
  const progress = useSharedValue(0);
  // Reanimated reads the system setting at app start; the AccessibilityInfo
  // query below catches a change made since. A ref, not state: it is only
  // read when the entrance would start, and must not cost a re-render.
  const reduceMotionAtLaunch = useReducedMotion();
  const reduceMotion = useRef(reduceMotionAtLaunch);
  // The entrance is over (or was skipped): render plain static styles from
  // here on, so the sheet's visibility no longer depends on Reanimated at
  // all. Reduced motion starts settled, so no animation ever runs.
  const [settled, setSettled] = useState(reduceMotionAtLaunch);
  const markSettled = useCallback(() => setSettled(true), []);

  useEffect(() => {
    AccessibilityInfo.isReduceMotionEnabled()
      .then((enabled) => {
        reduceMotion.current = enabled;
      })
      .catch(() => {});
  }, []);

  // The Modal's window is on screen. On iOS the content mounts before the
  // modal view controller is presented; an animation started then can be
  // lost, leaving the sheet present (its Close button findable) but invisible
  // — review #1. So the entrance starts here, straight from the event: no
  // state round-trip, no layout measurement.
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

  // Belt and braces: whatever happens to onShow or the animation, the sheet
  // is fully visible shortly after it mounts.
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_FALLBACK_MS);
    return () => clearTimeout(timer);
  }, []);

  // On settling, pin the shared value to its end too: a timing that stalled
  // part-way (Reanimated 4.4+ can drop UI-thread work for a view mounted
  // under JS-thread load — gorhom #2721) would otherwise hold the props it
  // last wrote, which override the static SETTLED_* styles.
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
          {/* The backdrop is a plain tap target, hidden from screen readers:
              the Close button is the accessible way out. */}
          <Pressable
            onPress={onClose}
            accessible={false}
            importantForAccessibility="no"
            style={{ flex: 1 }}
          />
        </Animated.View>
        {/* A sheet with a field (emoji search) must ride above the keyboard,
            on both platforms, the same way <Screen> does (#1246).
            keyboard-controller follows the keyboard inside a Modal's own
            window on Android too. Taps outside the card fall through to the
            backdrop. */}
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
                // Opaque: the sheet floats over the header and composer, and a
                // translucent card let them show through its buttons (#1193).
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
