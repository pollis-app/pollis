import { useState } from "react";
import { View, Text } from "react-native";
import { useRouter } from "expo-router";
import { Screen, Crumb, Field, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { useRequestOtp } from "../../hooks/queries/useAuth";

export default function AuthEmail() {
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
    <Screen>
      <Crumb segs={[{ label: "AUTH" }, { label: "Identify", leaf: true }]} />
      <View
        style={{ flex: 1, paddingHorizontal: 24, paddingTop: 30, gap: 24 }}
      >
        <View style={{ marginTop: 14, gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>Sign in</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            Enter your email. We'll send you a one-time code — no password.
          </Text>
        </View>
        <View style={{ gap: 8 }}>
          <Text style={ty.label}>EMAIL</Text>
          <Field
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
              "Couldn't send code. Check your connection and try again."}
          </Text>
        ) : null}
      </View>
      <BottomAction>
        <Button
          variant="primary"
          full
          onPress={onSubmit}
          disabled={requestOtp.isPending || !email.trim()}
          iconRight={<Icon.arrowRight color="#0a0907" />}
        >
          {requestOtp.isPending ? "SENDING…" : "CONTINUE"}
        </Button>
        {/* Recovery is reachable through the standard sign-in flow: enter
            your email, verify the OTP, and Pollis routes you to the
            recovery-key entry on a fresh device. No dedicated button
            needed. */}
      </BottomAction>
    </Screen>
  );
}
