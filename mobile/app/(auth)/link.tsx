import { useRef, useState } from "react";
import { Platform, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { CameraView, useCameraPermissions } from "expo-camera";
import { Screen, Crumb, Button, BottomAction, Field } from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { upper } from "../../i18n";
import { useClaimDeviceLink } from "../../hooks/queries";

const PAYLOAD_PREFIX = "pollis-link:v1:";

/**
 * Sign in with another device (#1207): scan the QR a signed-in device shows
 * under Settings → Security → Link a new device — or paste its code. A claim
 * signs this phone in with an enrollment-only session; the enrollment screen
 * then waits for Approve on the device that showed the QR.
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
      <Crumb
        segs={[
          { label: upper(t("mobile:auth.crumb.auth")) },
          { label: t("link.useDevice"), leaf: true },
        ]}
      />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 24, gap: 16 }}>
        <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 14, color: semantic.mute, lineHeight: 20 }}>
          {manual ? t("link.intro") : t("link.scanIntro")}
        </Text>

        {!manual && permission?.granted ? (
          <View style={{ aspectRatio: 1, width: "100%", overflow: "hidden", borderWidth: 1, borderColor: semantic.hair }}>
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
          <View style={{ gap: 10 }}>
            <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 13, color: semantic.mute }}>
              {t("link.cameraNeeded")}
            </Text>
            <Button testID="btn-link-allow-camera" onPress={() => void requestPermission()}>
              {upper(t("link.allowCamera"))}
            </Button>
          </View>
        ) : null}

        {manual ? (
          <View style={{ gap: 8 }}>
            <Text style={ty.label}>{upper(t("link.codeLabel"))}</Text>
            <Field
              testID="input-link-code"
              accessibilityLabel={t("link.codeLabel")}
              value={code}
              onChangeText={setCode}
              placeholder="pollis-link:v1:…"
            />
          </View>
        ) : null}

        {claim.isPending ? (
          <Text testID="link-claiming" style={{ fontFamily: ty.body.fontFamily, fontSize: 13, color: semantic.mute }}>
            {t("link.signingIn")}
          </Text>
        ) : null}
        {error ? (
          <Text testID="link-error" style={{ fontFamily: ty.body.fontFamily, fontSize: 13, color: semantic.danger }}>
            {error}
          </Text>
        ) : null}
      </View>
      <BottomAction>
        {manual ? (
          <Button testID="btn-link-submit" variant="primary" full disabled={!code.trim() || claim.isPending} onPress={() => submit(code)}>
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
        <Button testID="btn-link-use-email" variant="subtle" full onPress={() => router.back()}>
          {upper(t("link.useEmail"))}
        </Button>
      </BottomAction>
    </Screen>
  );
}
