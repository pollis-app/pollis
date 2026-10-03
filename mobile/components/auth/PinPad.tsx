import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { palette, semantic, fonts, r } from "../../theme/tokens";

const SUBS = ["", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV", "WXYZ"];
const KEYS = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "", "0", "bk"];

/** The four PIN cells: filled dots, a cursor on the next cell. */
export function PinCells({ length, size = 4 }: { length: number; size?: number }) {
  return (
    <View style={{ flexDirection: "row", gap: 14, justifyContent: "center" }}>
      {Array.from({ length: size }, (_, i) => {
        const filled = i < length;
        const cursor = i === length;
        return (
          <View
            key={i}
            style={{
              width: 52,
              height: 60,
              borderWidth: 1,
              borderRadius: r.sm,
              borderColor: filled || cursor ? semantic.accent : semantic.hairStrong,
              backgroundColor: semantic.fieldBg,
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            {filled ? (
              <Text style={{ fontFamily: fonts.sora500, fontSize: 24, color: semantic.ink }}>•</Text>
            ) : cursor ? (
              <View style={{ width: 2, height: 18, backgroundColor: semantic.accent }} />
            ) : null}
          </View>
        );
      })}
    </View>
  );
}

/**
 * The full-width numeric keypad the PIN screens use. Keys carry
 * `btn-pin-<digit>` / `btn-pin-back` testIDs, which the Maestro PIN subflow
 * taps.
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
      style={{
        flexDirection: "row",
        flexWrap: "wrap",
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
        backgroundColor: semantic.hairSoft,
        opacity: disabled ? 0.5 : 1,
      }}
      pointerEvents={disabled ? "none" : "auto"}
    >
      {KEYS.map((k, i) => (
        <Pressable
          key={i}
          disabled={k === ""}
          testID={k === "" ? undefined : k === "bk" ? "btn-pin-back" : `btn-pin-${k}`}
          accessibilityRole={k === "" ? undefined : "button"}
          accessibilityLabel={k === "" ? undefined : k === "bk" ? t("keys.delete") : k}
          onPress={() => (k === "bk" ? onBackspace() : onDigit(k))}
          // Subtle press feedback: the key briefly fills with the soft amber
          // tier so a tap is acknowledged without anything flashy.
          style={({ pressed }) => ({
            width: "33.333%",
            backgroundColor: pressed && k !== "" ? semantic.accentSoft : palette.bg,
            paddingVertical: 18,
            alignItems: "center",
            justifyContent: "center",
            gap: 2,
            marginBottom: 1,
          })}
        >
          <Text
            style={{
              fontFamily: fonts.sora400,
              fontSize: k === "bk" ? 26 : 22,
              color: k === "bk" ? semantic.ink2 : semantic.ink,
            }}
          >
            {k === "bk" ? "⌫" : k}
          </Text>
          {k && k !== "bk" ? (
            <Text style={{ fontFamily: fonts.sora400, fontSize: 9, letterSpacing: 1.8, color: semantic.mute }}>
              {SUBS[Number(k) - 1] || " "}
            </Text>
          ) : (
            <Text style={{ fontSize: 9 }}> </Text>
          )}
        </Pressable>
      ))}
    </View>
  );
}
