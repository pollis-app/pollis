import { ActivityIndicator, View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, fonts, layout, currentTheme, type as ty } from "../../theme/tokens";
import { controlDisabled } from "../../theme/button";
import { Icon } from "../icons";
import { haptic } from "../../lib/haptics";

const KEY = 72;
const COL_GAP = 24;
const ROW_GAP = 14;
const DOT = 16;

const ROWS = [
  ["1", "2", "3"],
  ["4", "5", "6"],
  ["7", "8", "9"],
  ["", "0", "bk"],
];

/** The PIN dots: filled vs hollow, so the count reads by shape, not colour. */
export function PinCells({ length, size = 4 }: { length: number; size?: number }) {
  const { t } = useTranslation("mobile");
  return (
    <View
      accessible
      accessibilityLabel={t("auth.pin.progress", { entered: length, total: size })}
      style={{
        flexDirection: "row",
        gap: 20,
        justifyContent: "center",
        alignItems: "center",
        minHeight: layout.touchMin,
      }}
    >
      {Array.from({ length: size }, (_, i) => {
        const filled = i < length;
        return (
          <View
            key={i}
            style={{
              width: DOT,
              height: DOT,
              borderRadius: DOT / 2,
              borderWidth: 2,
              borderColor: filled ? semantic.text : semantic.edge,
              backgroundColor: filled ? semantic.text : "transparent",
            }}
          />
        );
      })}
    </View>
  );
}

/**
 * Shown under the dots while the slow work after the fourth digit runs
 * (Argon2id, opening the encrypted DB, a DS round trip), alongside a disabled
 * keypad.
 */
export function PinBusy({ label, testID = "pin-busy" }: { label: string; testID?: string }) {
  return (
    <View
      testID={testID}
      // A real native view so e2e flows can find it by id (Fabric would
      // otherwise flatten a background-less wrapper away).
      collapsable={false}
      accessible
      accessibilityLabel={label}
      accessibilityState={{ busy: true }}
      accessibilityLiveRegion="polite"
      style={{
        flexDirection: "row",
        alignItems: "center",
        justifyContent: "center",
        gap: 10,
        minHeight: layout.touchMin,
      }}
    >
      <ActivityIndicator color={semantic.accent} />
      <Text style={ty.secondary}>{label}</Text>
    </View>
  );
}

/** The PIN keypad. Keys carry `btn-pin-<digit>` / `btn-pin-back` testIDs, which the Maestro PIN subflow taps. */
export function PinKeypad({
  onDigit,
  onBackspace,
  disabled,
}: {
  onDigit: (digit: string) => void;
  onBackspace: () => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation("common");
  return (
    <View
      // Must stay a real native view: Fabric flattens it while enabled and
      // re-creates it when pointerEvents flips to "none" (busy), which on
      // Android crashed PIN submit ("Unable to find viewState for tag N").
      collapsable={false}
      style={{
        alignItems: "center",
        gap: ROW_GAP,
        paddingTop: 12,
        paddingBottom: 20,
      }}
      pointerEvents={disabled ? "none" : "auto"}
    >
      {ROWS.map((row, ri) => (
        <View key={ri} style={{ flexDirection: "row", gap: COL_GAP }}>
          {row.map((k, ki) => {
            if (k === "") {
              return <View key={ki} style={{ width: KEY, height: KEY }} />;
            }
            const back = k === "bk";
            return (
              <Pressable
                key={ki}
                testID={back ? "btn-pin-back" : `btn-pin-${k}`}
                accessibilityRole="button"
                accessibilityLabel={back ? t("keys.delete") : k}
                accessibilityState={{ disabled: !!disabled }}
                // On touch-down, not release: a digit is undone by delete at
                // no cost, so the pad answers the moment a finger lands.
                onPressIn={() => {
                  haptic("keyTap");
                  if (back) {
                    onBackspace();
                  } else {
                    onDigit(k);
                  }
                }}
                style={({ pressed }) => ({
                  width: KEY,
                  height: KEY,
                  borderRadius: KEY / 2,
                  alignItems: "center",
                  justifyContent: "center",
                  // No border; disabled (busy) is a solid fill + dim digits,
                  // never a faded pad. Delete has no disc in any state.
                  backgroundColor: back
                    ? pressed && !disabled
                      ? semantic.accentSoft
                      : "transparent"
                    : disabled
                      ? controlDisabled(currentTheme()).fill
                      : pressed
                        ? semantic.accentSoft
                        : semantic.raised,
                })}
              >
                {back ? (
                  <Icon.backspace size={26} color={semantic.dim} />
                ) : (
                  <Text
                    maxFontSizeMultiplier={1.4}
                    style={{
                      fontFamily: fonts.medium,
                      fontSize: 28,
                      color: disabled ? semantic.dim : semantic.text,
                    }}
                  >
                    {k}
                  </Text>
                )}
              </Pressable>
            );
          })}
        </View>
      ))}
    </View>
  );
}
