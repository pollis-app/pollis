import { useRef, useState } from "react";
import { View, Text, TextInput, Pressable } from "react-native";
import { useRouter, useLocalSearchParams } from "expo-router";
import { KeyboardController } from "react-native-keyboard-controller";
import { Trans, useTranslation } from "react-i18next";
import { Screen, Header, Body, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic, fonts, r } from "../../theme/tokens";
import { useVerifyOtp } from "../../hooks/queries/useAuth";
import { useSubmitLock } from "../../hooks/useSubmitLock";
import { reopenKeyboard } from "../../lib/codeFieldFocus";
import { useThemeRerender } from "../../components/theme";

const CODE_LENGTH = 6;
// U+2060 so Android never strands the closing "." on its own line.
const WORD_JOINER = "\u2060";

export default function AuthOTP() {
  useThemeRerender();
  const { t } = useTranslation("auth");
  const router = useRouter();
  const { email: emailParam } = useLocalSearchParams<{ email?: string }>();
  const email = (emailParam ?? "").trim();
  const [code, setCode] = useState("");
  const [focused, setFocused] = useState(true);
  const input = useRef<TextInput>(null);
  const cells = Array.from({ length: CODE_LENGTH });
  const verifyOtp = useVerifyOtp();
  // The slow sign-in step: a DS round trip, and key generation for a new account.
  const submit = useSubmitLock();

  const onSubmit = () => {
    if (code.length !== CODE_LENGTH || !email || !submit.begin()) {
      return;
    }
    verifyOtp.mutate(
      { email, code },
      {
        onSuccess: (profile) => {
          // A new device must enroll before any key material exists.
          if (profile.enrollment_required) {
            router.push("/(auth)/enrollment");
            return;
          }
          router.push("/(auth)/pin");
        },
        onError: submit.end,
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
              values={{ email: `${email || t("mobile:auth.otp.yourEmail")}${WORD_JOINER}` }}
              components={{
                address: <Text style={{ fontFamily: fonts.semibold, color: semantic.text }} />,
              }}
            />
          }
        />

        <View testID="otp-code-field">
          {/* A picture of the hidden input; a tap reopens the keyboard (lib/codeFieldFocus). Hidden from screen readers. */}
          <Pressable
            onPress={() => reopenKeyboard(input.current, KeyboardController.isVisible())}
            accessible={false}
            importantForAccessibility="no-hide-descendants"
            accessibilityElementsHidden
            style={{ flexDirection: "row", gap: 8, justifyContent: "center" }}
          >
            {cells.map((_, i) => {
              const filled = i < code.length;
              const active =
                focused && (i === code.length || (code.length === CODE_LENGTH && i === CODE_LENGTH - 1));
              return (
                <View
                  key={i}
                  style={{
                    flex: 1,
                    maxWidth: 52,
                    minHeight: 60,
                    borderRadius: r.md,
                    backgroundColor: active ? semantic.accentSoft : semantic.raised,
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
          {/* opacity:0 proxy, autofocused; it takes no touches. */}
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
            style={{ position: "absolute", opacity: 0, pointerEvents: "none" }}
          />
        </View>

        {verifyOtp.isError ? (
          <AuthError
            center
            testID="text-auth-error"
            offerDiagnostics
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
          disabled={code.length !== CODE_LENGTH}
          loading={submit.busy}
          iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
        >
          {submit.busy ? t("otp.verifying") : t("otp.verify")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
