import { useState } from "react";
import { Linking, View, Text } from "react-native";
import { useRouter } from "expo-router";
import { Trans, useTranslation } from "react-i18next";
import { Screen, Body, Field, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic, type as ty } from "../../theme/tokens";
import { useRequestOtp } from "../../hooks/queries/useAuth";

export default function AuthEmail() {
  const { t } = useTranslation("auth");
  const router = useRouter();
  const [email, setEmail] = useState("");
  const requestOtp = useRequestOtp();

  const onSubmit = () => {
    const trimmed = email.trim();
    if (!trimmed) {
      return;
    }
    requestOtp.mutate(trimmed, {
      onSuccess: () => {
        router.push({ pathname: "/(auth)/otp", params: { email: trimmed } });
      },
    });
  };

  const linkStyle = {
    color: semantic.accent,
    textDecorationLine: "underline" as const,
  };

  return (
    <Screen testID="screen-auth-email" centered>
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 28 }}>
        <Heading title={t("mobile:auth.email.title")} subtitle={t("mobile:auth.email.intro")} />
        <View style={{ gap: 8 }}>
          <Text style={ty.section}>{t("otp.emailLabel")}</Text>
          <Field
            testID="input-email"
            accessibilityLabel={t("otp.emailLabel")}
            value={email}
            onChangeText={setEmail}
            keyboardType="email-address"
            autoComplete="email"
            textContentType="emailAddress"
            autoCorrect={false}
            placeholder={t("otp.emailPlaceholder")}
            icon={<Icon.mail size={18} color={semantic.muted} />}
          />
        </View>
        {requestOtp.isError ? (
          <AuthError
            message={(requestOtp.error as Error).message || t("mobile:auth.email.sendFailed")}
          />
        ) : null}
      </Body>
      <BottomAction>
        <Button
          testID="btn-submit-email"
          accessibilityLabel={t("otp.continue")}
          variant="primary"
          full
          onPress={onSubmit}
          disabled={requestOtp.isPending || !email.trim()}
          iconRight={<Icon.arrowRight size={18} color={semantic.onAccent} />}
        >
          {requestOtp.isPending ? t("otp.sending") : t("otp.continue")}
        </Button>
        {/* Agreeing to the terms is part of creating the account (#1213; App
            Store 1.2): the terms say abusive content is not tolerated. */}
        <Text testID="text-legal" style={[ty.meta, { textAlign: "center" }]}>
          <Trans
            t={t}
            i18nKey="auth:legal.agree"
            components={{
              terms: (
                <Text
                  accessibilityRole="link"
                  style={linkStyle}
                  onPress={() => void Linking.openURL("https://pollis.com/terms")}
                />
              ),
              privacy: (
                <Text
                  accessibilityRole="link"
                  style={linkStyle}
                  onPress={() => void Linking.openURL("https://pollis.com/privacy")}
                />
              ),
            }}
          />
        </Text>
        {/* QR device link (#1207): scan the code a signed-in device shows,
            instead of the email code. Recovery needs no button of its own: a
            fresh device that verifies the email code is routed to enrollment,
            which offers the recovery key. */}
        <Button
          testID="btn-sign-in-with-device"
          variant="secondary"
          full
          onPress={() => router.push("/(auth)/link")}
          icon={<Icon.device size={18} color={semantic.text} />}
        >
          {t("link.useDevice")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
