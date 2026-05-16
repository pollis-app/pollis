import { useRef, useState } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { Screen, Crumb, Button, BottomAction } from "../../components/ui";
import { PollisMark } from "../../components/PollisMark";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r } from "../../theme/tokens";

export default function AuthOTP() {
  const router = useRouter();
  const [code, setCode] = useState("472");
  const input = useRef<TextInput>(null);
  const cells = Array.from({ length: 6 });

  return (
    <Screen>
      <Crumb
        segs={[{ label: "AUTH" }, { label: "Verify email", leaf: true }]}
        end="04:38"
      />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 30, gap: 22 }}>
        <PollisMark />
        <View style={{ gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>Check your email</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            We sent a 6-digit code to{" "}
            <Text style={{ color: semantic.ink2 }}>dan@example.io</Text>. Enter
            it to continue.
          </Text>
        </View>

        <Pressable
          onPress={() => input.current?.focus()}
          style={{
            flexDirection: "row",
            gap: 10,
            justifyContent: "center",
            paddingVertical: 10,
          }}
        >
          {cells.map((_, i) => {
            const filled = i < code.length;
            const cursor = i === code.length;
            return (
              <View
                key={i}
                style={{
                  width: 44,
                  height: 56,
                  borderWidth: 1,
                  borderRadius: r.sm,
                  borderColor:
                    filled || cursor ? semantic.accent : semantic.hairStrong,
                  backgroundColor: filled
                    ? semantic.accentSoft
                    : semantic.fieldBg,
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                {filled ? (
                  <Text
                    style={{
                      fontFamily: ty.h1.fontFamily,
                      fontSize: 22,
                      color: semantic.ink,
                    }}
                  >
                    {code[i]}
                  </Text>
                ) : cursor ? (
                  <View
                    style={{
                      width: 2,
                      height: 24,
                      backgroundColor: semantic.accent,
                    }}
                  />
                ) : null}
              </View>
            );
          })}
        </Pressable>
        <TextInput
          ref={input}
          value={code}
          onChangeText={(v) => setCode(v.replace(/[^0-9]/g, "").slice(0, 6))}
          keyboardType="number-pad"
          style={{ position: "absolute", opacity: 0 }}
        />

        <View
          style={{
            flexDirection: "row",
            justifyContent: "space-between",
            alignItems: "center",
          }}
        >
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            Didn't receive it?
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              letterSpacing: 0.6,
              color: semantic.accent,
            }}
          >
            RESEND IN 04:38
          </Text>
        </View>

        <Pressable
          onPress={() => router.back()}
          style={{ flexDirection: "row", alignItems: "center", gap: 10 }}
        >
          <Icon.mail color={semantic.mute} />
          <Text
            style={{
              flex: 1,
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            Use a different email
          </Text>
          <Icon.fwd color={semantic.mute} />
        </Pressable>
      </View>

      <BottomAction>
        <Button
          variant="primary"
          full
          onPress={() => router.push("/(auth)/pin")}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          VERIFY
        </Button>
      </BottomAction>
    </Screen>
  );
}
