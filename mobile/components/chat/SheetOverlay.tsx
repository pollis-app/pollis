import { useEffect, useRef, useState } from "react";
import { Animated, Easing, Pressable } from "react-native";
import { semantic } from "../../theme/tokens";

// Entry timing (#1193): in line with the ~200 ms stack transitions — fast
// enough to read as a response to the tap, not a presentation.
const ENTER_MS = 180;

/**
 * Full-screen dimmed backdrop with a bottom-anchored card — the long-press
 * action-sheet pattern. Tapping the backdrop dismisses; taps inside the card
 * are swallowed. On mount the backdrop fades in and the card slides up from
 * below its own height; dismissal is immediate, since its actions usually
 * navigate away and must not wait on an exit animation.
 */
export function SheetOverlay({
  onClose,
  children,
}: {
  onClose: () => void;
  children: React.ReactNode;
}) {
  const progress = useRef(new Animated.Value(0)).current;
  // The card's own height, so it starts exactly below the screen edge
  // whatever its content. Until measured it is held fully off-screen.
  const [cardHeight, setCardHeight] = useState(0);

  useEffect(() => {
    if (cardHeight === 0) {
      return;
    }
    Animated.timing(progress, {
      toValue: 1,
      duration: ENTER_MS,
      easing: Easing.out(Easing.cubic),
      useNativeDriver: true,
    }).start();
  }, [cardHeight, progress]);

  return (
    <Pressable
      onPress={onClose}
      // Neither wrapper has its own accessibilityLabel, so leaving them
      // `accessible` (Pressable's default) makes iOS collapse every
      // child button below into ONE opaque compound element — a
      // VoiceOver user could never reach the actions individually.
      accessible={false}
      style={{
        position: "absolute",
        top: 0,
        bottom: 0,
        left: 0,
        right: 0,
        justifyContent: "flex-end",
      }}
    >
      <Animated.View
        pointerEvents="none"
        style={{
          position: "absolute",
          top: 0,
          bottom: 0,
          left: 0,
          right: 0,
          backgroundColor: "rgba(0,0,0,0.55)",
          opacity: progress,
        }}
      />
      <Animated.View
        onLayout={(e) => {
          if (cardHeight === 0) {
            setCardHeight(e.nativeEvent.layout.height);
          }
        }}
        style={{
          opacity: cardHeight === 0 ? 0 : 1,
          transform: [
            {
              translateY: progress.interpolate({
                inputRange: [0, 1],
                outputRange: [cardHeight, 0],
              }),
            },
          ],
        }}
      >
        <Pressable
          onPress={(e) => e.stopPropagation()}
          accessible={false}
          style={{
            // Opaque: the sheet floats over the header and composer, and a
            // translucent card let them show through its buttons (#1193).
            backgroundColor: semantic.sheetBg,
            borderTopWidth: 1,
            borderTopColor: semantic.hair,
            paddingHorizontal: 18,
            paddingTop: 14,
            paddingBottom: 30,
            gap: 10,
          }}
        >
          {children}
        </Pressable>
      </Animated.View>
    </Pressable>
  );
}
