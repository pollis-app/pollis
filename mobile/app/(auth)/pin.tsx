import { useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Screen, Crumb } from "../../components/ui";
import { PollisMark } from "../../components/PollisMark";
import { palette, semantic, type as ty, fonts, r } from "../../theme/tokens";

const SUBS = ["", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV", "WXYZ"];

export default function AuthPIN() {
  const router = useRouter();
  const [pin, setPin] = useState("");

  const push = (n: string) => {
    if (pin.length >= 4) {
      return;
    }
    const next = pin + n;
    setPin(next);
    if (next.length === 4) {
      setTimeout(() => router.replace("/(auth)/initializing"), 150);
    }
  };

  const keys = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "", "0", "bk"];

  return (
    <Screen>
      <Crumb segs={[{ label: "AUTH" }, { label: "Set device PIN", leaf: true }]} />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 24, gap: 18 }}>
        <PollisMark />
        <View style={{ gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>New device PIN</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            Used to unlock Pollis on this device. This stays on your phone — we
            never see it.
          </Text>
        </View>

        <View style={{ paddingVertical: 14 }}>
          <View
            style={{ flexDirection: "row", gap: 14, justifyContent: "center" }}
          >
            {[0, 1, 2, 3].map((i) => {
              const filled = i < pin.length;
              const cursor = i === pin.length;
              return (
                <View
                  key={i}
                  style={{
                    width: 52,
                    height: 60,
                    borderWidth: 1,
                    borderRadius: r.sm,
                    borderColor:
                      filled || cursor
                        ? semantic.accent
                        : semantic.hairStrong,
                    backgroundColor: semantic.fieldBg,
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  {filled ? (
                    <Text
                      style={{
                        fontFamily: fonts.sora500,
                        fontSize: 24,
                        color: semantic.ink,
                      }}
                    >
                      •
                    </Text>
                  ) : cursor ? (
                    <View
                      style={{
                        width: 2,
                        height: 18,
                        backgroundColor: semantic.accent,
                      }}
                    />
                  ) : null}
                </View>
              );
            })}
          </View>
          <Text
            style={[ty.label, { textAlign: "center", marginTop: 14 }]}
          >
            STEP 1 OF 2 · ENTER PIN
          </Text>
        </View>
      </View>

      <View
        style={{
          flexDirection: "row",
          flexWrap: "wrap",
          borderTopWidth: 1,
          borderTopColor: semantic.hairSoft,
          backgroundColor: semantic.hairSoft,
        }}
      >
        {keys.map((k, i) => (
          <Pressable
            key={i}
            disabled={k === ""}
            onPress={() => (k === "bk" ? setPin(pin.slice(0, -1)) : push(k))}
            style={{
              width: "33.333%",
              backgroundColor: palette.bg,
              paddingVertical: 18,
              alignItems: "center",
              justifyContent: "center",
              gap: 2,
              marginBottom: 1,
            }}
          >
            <Text
              style={{
                fontFamily: fonts.sora400,
                fontSize: k === "bk" ? 18 : 22,
                color: k === "bk" ? semantic.ink2 : semantic.ink,
              }}
            >
              {k === "bk" ? "⌫" : k}
            </Text>
            {k && k !== "bk" ? (
              <Text
                style={{
                  fontFamily: fonts.sora400,
                  fontSize: 9,
                  letterSpacing: 1.8,
                  color: semantic.mute,
                }}
              >
                {SUBS[Number(k) - 1] || " "}
              </Text>
            ) : (
              <Text style={{ fontSize: 9 }}> </Text>
            )}
          </Pressable>
        ))}
      </View>
    </Screen>
  );
}
