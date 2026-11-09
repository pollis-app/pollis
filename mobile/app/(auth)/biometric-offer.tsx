import { useEffect, useState } from "react";
import { Platform, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Screen, Body, Button, BottomAction } from "../../components/ui";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { Icon } from "../../components/icons";
import { semantic } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import { biometricMethod, enableDeviceUnlock, markOffered } from "../../lib/deviceUnlock";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

/**
 * The one-time offer after a PIN unlock (#1685): "Unlock with Face ID?". Shown once per user and
 * device (recorded as it opens, so a kill here never brings it back), off unless accepted. Turning
 * it on here needs no second PIN: core accepts the PIN just typed to unlock, once, for a few
 * minutes. Either answer continues into the app.
 */
function BiometricOffer() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const user = appStore.currentUser;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const method = biometricMethod();

  useEffect(() => {
    if (user) {
      void markOffered(user.id);
    }
  }, [user]);

  const proceed = () => router.replace("/(auth)/initializing");

  const onAccept = async () => {
    if (!user || busy) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await enableDeviceUnlock(user.id, { readBack: true });
      haptic("success");
      proceed();
    } catch (e) {
      haptic("error");
      setError((e as Error).message || t("biometric.failed"));
      setBusy(false);
    }
  };

  return (
    <Screen testID="screen-biometric-offer" centered>
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 52, gap: 24 }}>
        <View style={{ alignItems: "center", paddingBottom: 8 }}>
          {Platform.OS === "ios" ? (
            <Icon.faceId size={56} color={semantic.accent} />
          ) : (
            <Icon.fingerprint size={56} color={semantic.accent} />
          )}
        </View>
        <Heading
          testID="biometric-offer-heading"
          title={t("biometric.offerTitle", { method })}
          subtitle={t("biometric.offerBody", { method })}
        />
        {error ? <AuthError testID="biometric-offer-error" message={error} /> : null}
      </Body>
      <BottomAction>
        <View style={{ gap: 8 }}>
          <Button
            testID="btn-biometric-offer-accept"
            full
            variant="primary"
            disabled={busy}
            onPress={() => void onAccept()}
          >
            {busy ? t("biometric.turningOn") : t("biometric.offerAccept")}
          </Button>
          <Button
            testID="btn-biometric-offer-decline"
            full
            variant="subtle"
            disabled={busy}
            onPress={proceed}
          >
            {t("biometric.offerDecline")}
          </Button>
        </View>
      </BottomAction>
    </Screen>
  );
}

export default observer(BiometricOffer);
