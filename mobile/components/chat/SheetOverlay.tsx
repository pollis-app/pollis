import { useEffect, useRef, useState } from "react";
import {
  Animated,
  Easing,
  Modal,
  Pressable,
  Text,
  View,
  AccessibilityInfo,
  KeyboardAvoidingView,
  Platform,
} from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, type as ty, r, space } from "../../theme/tokens";
import { useTheme } from "../theme";
import { Icon } from "../icons";
import { useBottomInset } from "../ui";
import { useAndroidKeyboardInset } from "../../hooks/useAndroidKeyboardInset";

// Entry timing (#1193): in line with the ~200 ms stack transitions — fast
// enough to read as a response to the tap, not a presentation.
const ENTER_MS = 180;
// Upper bound on the entrance: past this the sheet is shown statically.
const SETTLE_FALLBACK_MS = 600;

/**
 * Bottom sheet (Actions.dc.html). Rendered in a transparent `Modal`, so the
 * backdrop covers the WHOLE screen — status bar and home-indicator area
 * included — and screen-reader focus is confined to the sheet. The card is
 * anchored to the bottom edge, edge to edge, with a 20pt top radius and the
 * bottom safe-area inset as extra padding. No drag handle: the header row
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
  const androidKeyboard = useAndroidKeyboardInset();
  const progress = useRef(new Animated.Value(0)).current;
  // The card's own height, so it starts exactly below the screen edge
  // whatever its content. Until measured it is held fully off-screen.
  const [cardHeight, setCardHeight] = useState(0);
  const [reduceMotion, setReduceMotion] = useState(false);
  // The Modal's window is on screen (onShow). On iOS the content mounts
  // before the modal view controller is presented; a native-driven animation
  // started then can be lost, leaving the sheet present (its Close button
  // findable) but fully transparent — review #1. So the entrance waits for
  // onShow as well as the measurement.
  const [shown, setShown] = useState(false);
  // The entrance is over (or was skipped): render plain static styles from
  // here on, so no later re-render can leave the sheet at an animated 0.
  const [settled, setSettled] = useState(false);

  useEffect(() => {
    AccessibilityInfo.isReduceMotionEnabled()
      .then(setReduceMotion)
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (cardHeight === 0 || !shown || settled) {
      return;
    }
    Animated.timing(progress, {
      toValue: 1,
      duration: reduceMotion ? 0 : ENTER_MS,
      easing: Easing.out(Easing.cubic),
      useNativeDriver: true,
    }).start(() => setSettled(true));
  }, [cardHeight, shown, settled, progress, reduceMotion]);

  // Belt and braces: whatever happens to onShow, onLayout or the animation,
  // the sheet is fully visible shortly after it mounts.
  useEffect(() => {
    const timer = setTimeout(() => setSettled(true), SETTLE_FALLBACK_MS);
    return () => clearTimeout(timer);
  }, []);

  return (
    <Modal
      visible
      transparent
      animationType="none"
      statusBarTranslucent
      navigationBarTranslucent
      onRequestClose={onClose}
      onShow={() => setShown(true)}
    >
      <View style={{ flex: 1 }}>
        <Animated.View
          style={{
            position: "absolute",
            top: 0,
            bottom: 0,
            start: 0,
            end: 0,
            backgroundColor: semantic.backdrop,
            opacity: settled ? 1 : progress,
          }}
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
        {/* A sheet with a field (emoji search) must ride above the keyboard:
            iOS via KeyboardAvoidingView, Android via the explicit inset (the
            same split as <Screen>). Taps outside the card fall through to the
            backdrop. */}
        <KeyboardAvoidingView
          pointerEvents="box-none"
          behavior={Platform.OS === "ios" ? "padding" : undefined}
          style={{ flex: 1, justifyContent: "flex-end", paddingBottom: androidKeyboard }}
        >
          <Animated.View
            testID={testID}
            accessibilityViewIsModal
            onLayout={(e) => {
              if (cardHeight === 0) {
                setCardHeight(e.nativeEvent.layout.height);
              }
            }}
            style={{
              opacity: settled || cardHeight > 0 ? 1 : 0,
              transform: settled
                ? []
                : [
                    {
                      translateY: progress.interpolate({
                        inputRange: [0, 1],
                        outputRange: [cardHeight, 0],
                      }),
                    },
                  ],
              // Opaque: the sheet floats over the header and composer, and a
              // translucent card let them show through its buttons (#1193).
              backgroundColor: semantic.sheetBg,
              borderTopStartRadius: r.sheet,
              borderTopEndRadius: r.sheet,
              borderTopWidth: 1,
              borderTopColor: semantic.hair,
              paddingHorizontal: space.xxl,
              paddingTop: space.xl,
              paddingBottom: bottomInset + space.lg,
              gap: space.lg,
            }}
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
