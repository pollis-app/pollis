import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { semantic, fonts, layout } from "../../theme/tokens";
import { Icon } from "../icons";

// Key diameter and the gaps between keys. 72pt circles keep every key well
// above the 44pt minimum and leave room for 28pt digits.
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

/**
 * The PIN progress dots. A filled dot is a solid disc; an empty one is a
 * hollow ring — so the count reads by shape, not colour. Spoken as one
 * element ("2 of 4 digits entered").
 */
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
 * The numeric keypad the PIN screens use: 72pt round keys on the raised
 * surface with 28pt digits. Keys carry `btn-pin-<digit>` / `btn-pin-back`
 * testIDs, which the Maestro PIN subflow taps, and speak as their digit or
 * "Delete".
 */
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
      // Must stay a real native view. With no background or border, Fabric
      // flattens it away while enabled and re-creates it when pointerEvents
      // flips to "none" (busy); on Android that churn races view
      // preallocation and the next padding update crashes the app ("Unable
      // to find viewState for tag N" in updatePadding) on PIN submit.
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
                onPress={() => (back ? onBackspace() : onDigit(k))}
                // A tap briefly fills the key with the soft accent tier: an
                // acknowledgement without anything flashy.
                style={({ pressed }) => ({
                  width: KEY,
                  height: KEY,
                  borderRadius: KEY / 2,
                  alignItems: "center",
                  justifyContent: "center",
                  // Disabled (busy): solid dim digits and a dashed edge ring
                  // instead of fading the pad — review #3/#4.
                  backgroundColor: disabled
                    ? "transparent"
                    : pressed
                      ? semantic.accentSoft
                      : back
                        ? "transparent"
                        : semantic.raised,
                  borderWidth: disabled && !back ? 1 : 0,
                  borderColor: semantic.edge,
                  borderStyle: "dashed",
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
