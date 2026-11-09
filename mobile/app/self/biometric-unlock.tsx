import { useState } from "react";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { useQueryClient } from "@tanstack/react-query";
import { PinEntryPage } from "../../components/self/PinEntryPage";
import { appStore } from "../../stores/appStore";
import { biometricMethod, enableDeviceUnlock } from "../../lib/deviceUnlock";
import { DEVICE_UNLOCK_QUERY_KEY } from "../../hooks/queries/useDeviceUnlock";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

/**
 * Security → "Unlock with Face ID" switched on (#1685): turning it on is a security change, so it
 * asks for the PIN (a wrong one counts toward the lockout like any other). Then the system prompt
 * stores the key (Android) or reads it back once (iOS), and the page goes back.
 */
export default function BiometricUnlockSetup() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useRouter();
  const queryClient = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [round, setRound] = useState(0);
  const method = biometricMethod();

  const onComplete = async (pin: string) => {
    const user = appStore.currentUser;
    if (!user || busy) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await enableDeviceUnlock(user.id, { pin, readBack: true });
      haptic("success");
      await queryClient.invalidateQueries({ queryKey: DEVICE_UNLOCK_QUERY_KEY });
      router.back();
    } catch (e) {
      haptic("error");
      setError((e as Error).message || t("biometric.failed"));
      setRound((n) => n + 1);
      setBusy(false);
    }
  };

  return (
    <PinEntryPage
      testID="screen-biometric-unlock"
      title={t("biometric.setupTitle")}
      backTo={t("settings:security.title")}
      heading={t("biometric.confirmTitle")}
      subtitle={t("biometric.confirmBody", { method })}
      error={error}
      errorTestID="biometric-unlock-error"
      busy={busy}
      busyLabel={t("biometric.turningOn")}
      resetKey={round}
      onComplete={(pin) => void onComplete(pin)}
    />
  );
}
