import { useRef, useState } from "react";
import { Platform, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { CameraView, useCameraPermissions } from "expo-camera";
import * as Clipboard from "expo-clipboard";
import { Screen, Crumb, Button, BottomAction, Field } from "../../components/ui";
import { Heading } from "../../components/auth/Heading";
import { BackLink } from "../../components/auth/BackLink";
import { semantic, type as ty, r } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useClaimDeviceLink } from "../../hooks/queries";

const PAYLOAD_PREFIX = "pollis-link:v1:";

/**
 * Sign in with another device (#1207): scan the code a signed-in device shows
 * under Security → Link a new device, or paste it. Laid out like Sign in: a
 * title, one line, the camera (or one big Paste button), and a quiet way back
 * to email.
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

  const pasteAndSubmit = async () => {
    const text = (await Clipboard.getStringAsync()).trim();
    setCode(text);
    if (text.startsWith(PAYLOAD_PREFIX)) {
      submit(text);
    } else {
      setError(t("link.notACode"));
    }
  };

  return (
    <Screen testID="screen-auth-link" centered>
      <Crumb segs={[{ label: upper(t("mobile:auth.crumb.auth")) }, { label: t("link.useDevice"), leaf: true }]} />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32, gap: 28 }}>
        <Heading title={t("link.scanTitle")} subtitle={manual ? t("link.codeIntro") : t("link.scanIntro")} />

        {!manual && permission?.granted ? (
          <View style={{ aspectRatio: 1, width: "100%", overflow: "hidden", borderRadius: r.lg, borderWidth: 1, borderColor: semantic.hair }}>
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
          <Button testID="btn-link-allow-camera" variant="primary" full onPress={() => void requestPermission()}>
            {upper(t("link.allowCamera"))}
          </Button>
        ) : null}

        {manual ? (
          <View style={{ gap: 14 }}>
            <Button testID="btn-link-paste" variant="primary" full disabled={claim.isPending} onPress={() => void pasteAndSubmit()}>
              {upper(t("link.paste"))}
            </Button>
            <Field
              testID="input-link-code"
              accessibilityLabel={t("link.codeLabel")}
              value={code}
              onChangeText={setCode}
              placeholder={t("link.codePlaceholder")}
            />
          </View>
        ) : null}

        {claim.isPending ? (
          <Text testID="link-claiming" style={{ fontFamily: ty.body.fontFamily, fontSize: 14, color: semantic.mute, textAlign: "center" }}>
            {t("link.signingIn")}
          </Text>
        ) : null}
        {error ? (
          <Text testID="link-error" style={{ fontFamily: ty.body.fontFamily, fontSize: 14, color: semantic.danger }}>
            {error}
          </Text>
        ) : null}

        <BackLink testID="btn-link-use-email" label={t("link.useEmail")} onPress={() => router.back()} />
      </View>
      <BottomAction>
        {manual && code.trim().startsWith(PAYLOAD_PREFIX) ? (
          <Button testID="btn-link-submit" variant="primary" full disabled={claim.isPending} onPress={() => submit(code)}>
            {upper(t("link.signIn"))}
          </Button>
        ) : null}
        <Button
          testID="btn-link-toggle-manual"
          variant="subtle"
          full
          onPress={() => {
            setManual((m) => !m);
            setError(null);
          }}
        >
          {upper(manual ? t("link.scanInstead") : t("link.enterCode"))}
        </Button>
      </BottomAction>
    </Screen>
  );
}
