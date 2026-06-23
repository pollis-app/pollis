import { useRef, useState } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { Screen, Crumb, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { useVerifyOtp } from "../../hooks/queries/useAuth";

export default function AuthOTP() {
  const router = useRouter();
  const { email: emailParam } = useLocalSearchParams<{ email?: string }>();
  const email = (emailParam ?? "").trim();
  const [code, setCode] = useState("");
  const input = useRef<TextInput>(null);
  const cells = Array.from({ length: 6 });
  const verifyOtp = useVerifyOtp();

  const onSubmit = () => {
    if (code.length !== 6 || !email) {
      return;
    }
    verifyOtp.mutate(
      { email, code },
      {
        onSuccess: (profile) => {
          // Existing user signing in on a new device — needs to enroll
          // first (sibling-device approval or recovery key) before any
          // local key material exists for the PIN screen to wrap.
          if (profile.enrollment_required) {
            router.push("/(auth)/enrollment");
            return;
          }
          router.push("/(auth)/pin");
        },
      },
    );
  };

  return (
    <Screen>
      <Crumb
        segs={[{ label: "AUTH" }, { label: "Verify email", leaf: true }]}
      />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 30, gap: 22 }}>
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
            <Text style={{ color: semantic.ink2 }}>
              {email || "your email"}
            </Text>
            . Enter it to continue.
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
          autoFocus
          style={{ position: "absolute", opacity: 0 }}
        />

        {verifyOtp.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              textAlign: "center",
            }}
          >
            {(verifyOtp.error as Error).message ||
              "Invalid code. Please try again."}
          </Text>
        ) : null}

        <Pressable
          onPress={() => router.back()}
          style={{
            flexDirection: "row",
            alignItems: "center",
            alignSelf: "flex-start",
            gap: 8,
          }}
        >
          <Icon.back color={semantic.ink} />
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 16,
              color: semantic.ink,
            }}
          >
            Use a different email
          </Text>
        </Pressable>
      </View>

      <BottomAction>
        <Button
          variant="primary"
          full
          onPress={onSubmit}
          disabled={code.length !== 6 || verifyOtp.isPending}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {verifyOtp.isPending ? "VERIFYING…" : "VERIFY"}
        </Button>
      </BottomAction>
    </Screen>
  );
}
