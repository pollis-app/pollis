import { useRef, useState } from "react";
import { ActivityIndicator, Platform, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { CameraView, useCameraPermissions } from "expo-camera";
import { Screen, Header, Body, Button, BottomAction, Field } from "../../components/ui";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic, type as ty, fonts, r } from "../../theme/tokens";
import { useClaimDeviceLink } from "../../hooks/queries";

const PAYLOAD_PREFIX = "pollis-link:v1:";

/**
 * Sign in with another device (#1207): scan the code a signed-in device shows
 * under Security → Link a new device, or paste it into the field. Laid out
 * like Sign in: a back button to email, a title, one line, then the camera (or
 * the code field).
 */
export default function LinkSignIn() {
  const { t } = useTranslation("auth");
  const router = useRouter();
  const claim = useClaimDeviceLink();
  const [permission, requestPermission] = useCameraPermissions();
  const [manual, setManual] = useState(false);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  // The camera reports every frame the code is in view; claim once.
  const claimed = useRef(false);

  const submit = (payload: string) => {
    if (claimed.current || claim.isPending) {
      return;
    }
    claimed.current = true;
    setError(null);
    const deviceName = Platform.OS === "ios" ? "Pollis on iPhone" : "Pollis on Android";
    claim.mutate(
      { payload: payload.trim(), deviceName },
      {
        onSuccess: () => router.replace({ pathname: "/(auth)/enrollment", params: { linked: "1" } }),
        onError: (e) => {
          claimed.current = false;
          setError((e as Error).message || t("link.failed"));
        },
      },
    );
  };

  return (
    <Screen testID="screen-auth-link" centered>
      {/* Back to email. The id predates the top bar: the tour flow taps it
          to return from this screen. */}
      <Header
        bordered={false}
        backTestID="btn-link-use-email"
        backLabel={t("link.useEmail")}
      />
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 8, gap: 24 }}>
        <Heading title={t("link.scanTitle")} subtitle={manual ? t("link.codeIntro") : t("link.scanIntro")} />

        {!manual && permission?.granted ? (
          <View
            style={{
              aspectRatio: 1,
              width: "100%",
              overflow: "hidden",
              borderRadius: r.lg,
              backgroundColor: semantic.raised,
            }}
          >
            <CameraView
              testID="link-camera"
              style={{ flex: 1 }}
              facing="back"
              barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
              onBarcodeScanned={({ data }) => {
                if (data.startsWith(PAYLOAD_PREFIX)) {
                  submit(data);
                }
              }}
            />
          </View>
        ) : null}

        {!manual && permission && !permission.granted ? (
          <Button
            testID="btn-link-allow-camera"
            variant="primary"
            full
            onPress={() => void requestPermission()}
          >
            {t("link.allowCamera")}
          </Button>
        ) : null}

        {manual ? (
          <View style={{ gap: 8 }}>
            <Text style={ty.section}>{t("link.codeLabel")}</Text>
            <Field
              testID="input-link-code"
              accessibilityLabel={t("link.codeLabel")}
              value={code}
              onChangeText={setCode}
              autoCorrect={false}
              placeholder={t("link.codePlaceholder")}
              style={{ fontFamily: fonts.mono400 }}
            />
          </View>
        ) : null}

        {claim.isPending ? (
          <View
            style={{ flexDirection: "row", alignItems: "center", justifyContent: "center", gap: 10 }}
            accessibilityLiveRegion="polite"
          >
            <ActivityIndicator color={semantic.accent} />
            <Text testID="link-claiming" style={ty.secondary}>
              {t("link.signingIn")}
            </Text>
          </View>
        ) : null}
        {error ? <AuthError testID="link-error" message={error} /> : null}
      </Body>
      <BottomAction>
        {manual && code.trim().startsWith(PAYLOAD_PREFIX) ? (
          <Button
            testID="btn-link-submit"
            variant="primary"
            full
            disabled={claim.isPending}
            onPress={() => submit(code)}
          >
            {t("link.signIn")}
          </Button>
        ) : null}
        <Button
          testID="btn-link-toggle-manual"
          variant="secondary"
          full
          onPress={() => {
            setManual((m) => !m);
            setError(null);
          }}
        >
          {manual ? t("link.scanInstead") : t("link.enterCode")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
