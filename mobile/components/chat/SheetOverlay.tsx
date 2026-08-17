import { Pressable } from "react-native";
import { semantic } from "../../theme/tokens";

/**
 * Full-screen dimmed backdrop with a bottom-anchored card — the long-press
 * action-sheet pattern. Tapping the backdrop dismisses; taps inside the card
 * are swallowed.
 */
export function SheetOverlay({
  onClose,
  children,
}: {
  onClose: () => void;
  children: React.ReactNode;
}) {
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
        backgroundColor: "rgba(0,0,0,0.55)",
        justifyContent: "flex-end",
      }}
    >
      <Pressable
        onPress={(e) => e.stopPropagation()}
        accessible={false}
        style={{
          backgroundColor: semantic.cardBg,
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
    </Pressable>
  );
}
