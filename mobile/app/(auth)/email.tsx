import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Crumb, Field, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useRequestOtp } from "../../hooks/queries/useAuth";
import { upper } from "../../i18n";

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

  return (
    <Screen testID="screen-auth-email" centered>
      <Crumb
        segs={[
          { label: upper(t("mobile:auth.crumb.auth")) },
          { label: t("mobile:auth.crumb.identify"), leaf: true },
        ]}
      />
      <View
        style={{ flex: 1, paddingHorizontal: 24, paddingTop: 30, gap: 24 }}
      >
        <View style={{ marginTop: 14, gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>
            {t("mobile:auth.email.title")}
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            {t("mobile:auth.email.intro")}
          </Text>
        </View>
        <View style={{ gap: 8 }}>
          <Text style={ty.label}>{upper(t("otp.emailLabel"))}</Text>
          <Field
            testID="input-email"
            accessibilityLabel={t("otp.emailLabel")}
            amber
            value={email}
            onChangeText={setEmail}
            keyboardType="email-address"
            icon={<Icon.mail color={semantic.mute} />}
          />
        </View>
        {requestOtp.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
            }}
          >
            {(requestOtp.error as Error).message ||
              t("mobile:auth.email.sendFailed")}
          </Text>
        ) : null}
      </View>
      <BottomAction>
        <Button
          testID="btn-submit-email"
          accessibilityLabel={t("otp.continue")}
          variant="primary"
          full
          onPress={onSubmit}
          disabled={requestOtp.isPending || !email.trim()}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {upper(requestOtp.isPending ? t("otp.sending") : t("otp.continue"))}
        </Button>
        {/* Recovery is reachable through the standard sign-in flow: enter
            your email, verify the OTP, and Pollis routes you to the
            recovery-key entry on a fresh device. No dedicated button
            needed. */}
      </BottomAction>
    </Screen>
  );
}
