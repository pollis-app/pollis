import { useRef, useState } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { Trans, useTranslation } from "react-i18next";
import { Screen, Header, Body, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic, fonts, r } from "../../theme/tokens";
import { useVerifyOtp } from "../../hooks/queries/useAuth";

const CODE_LENGTH = 6;

export default function AuthOTP() {
  const { t } = useTranslation("auth");
  const router = useRouter();
  const { email: emailParam } = useLocalSearchParams<{ email?: string }>();
  const email = (emailParam ?? "").trim();
  const [code, setCode] = useState("");
  const [focused, setFocused] = useState(true);
  const input = useRef<TextInput>(null);
  const cells = Array.from({ length: CODE_LENGTH });
  const verifyOtp = useVerifyOtp();

  const onSubmit = () => {
    if (code.length !== CODE_LENGTH || !email) {
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
    <Screen testID="screen-auth-otp" centered>
      <Header
        bordered={false}
        backLabel={t("mobile:auth.otp.useDifferentEmail")}
      />
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 8, gap: 28 }}>
        <Heading
          title={t("mobile:auth.otp.title")}
          subtitle={
            <Trans
              t={t}
              i18nKey="mobile:auth.otp.sentTo"
              values={{ email: email || t("mobile:auth.otp.yourEmail") }}
              components={{
                address: <Text style={{ fontFamily: fonts.semibold, color: semantic.text }} />,
              }}
            />
          }
        />

        <View>
          {/* The six boxes are a picture of the hidden input below; a tap on
              them focuses it. Screen readers get the input itself, so the
              boxes are hidden from them. */}
          <Pressable
            onPress={() => input.current?.focus()}
            accessible={false}
            importantForAccessibility="no-hide-descendants"
            accessibilityElementsHidden
            style={{ flexDirection: "row", gap: 8, justifyContent: "center" }}
          >
            {cells.map((_, i) => {
              const filled = i < code.length;
              // The box the next digit lands in (the last box once full).
              const active =
                focused && (i === code.length || (code.length === CODE_LENGTH && i === CODE_LENGTH - 1));
              return (
                <View
                  key={i}
                  style={{
                    flex: 1,
                    maxWidth: 52,
                    minHeight: 60,
                    borderWidth: active ? 2 : 1,
                    borderRadius: r.md,
                    borderColor: active ? semantic.accent : semantic.edge,
                    backgroundColor: semantic.raised,
                    alignItems: "center",
                    justifyContent: "center",
                  }}
                >
                  {filled ? (
                    <Text style={{ fontFamily: fonts.semibold, fontSize: 28, color: semantic.text }}>
                      {code[i]}
                    </Text>
                  ) : active ? (
                    <View style={{ width: 2, height: 26, backgroundColor: semantic.accent }} />
                  ) : null}
                </View>
              );
            })}
          </Pressable>
          {/* input-otp is an opacity:0 proxy over the visual boxes,
              autofocused on mount — Maestro types into it without a tap. */}
          <TextInput
            ref={input}
            testID="input-otp"
            accessibilityLabel={t("mobile:auth.otp.codeLabel")}
            value={code}
            onChangeText={(v) => setCode(v.replace(/[^0-9]/g, "").slice(0, CODE_LENGTH))}
            onFocus={() => setFocused(true)}
            onBlur={() => setFocused(false)}
            keyboardType="number-pad"
            autoFocus
            style={{ position: "absolute", opacity: 0 }}
          />
        </View>

        {verifyOtp.isError ? (
          <AuthError
            center
            message={(verifyOtp.error as Error).message || t("mobile:auth.otp.invalidCode")}
          />
        ) : null}
      </Body>

      <BottomAction>
        <Button
          testID="btn-submit-otp"
          accessibilityLabel={t("otp.verify")}
          variant="primary"
          full
          onPress={onSubmit}
          disabled={code.length !== CODE_LENGTH || verifyOtp.isPending}
          iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
        >
          {verifyOtp.isPending ? t("otp.verifying") : t("otp.verify")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
