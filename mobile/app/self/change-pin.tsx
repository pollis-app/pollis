import { useRef, useState } from "react";
import { View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, Button, BottomAction } from "../../components/ui";
import { Heading } from "../../components/auth/Heading";
import { PinEntryPage } from "../../components/self/PinEntryPage";
import { useSetPin } from "../../hooks/queries/useAuth";
import { useThemeRerender } from "../../components/theme";

type Step = "old" | "new" | "confirm";

/**
 * Change PIN (desktop's ChangePinPage, #1685 closes the parity gap): current, new, confirm. Core
 * checks the current PIN (a wrong one counts toward the lockout) and re-wraps the keys; biometric
 * unlock, where on, keeps working because it wraps the keys, not the PIN.
 */
export default function ChangePin() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const router = useRouter();
  const setPin = useSetPin();
  const [step, setStep] = useState<Step>("old");
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  // Bumped to clear the dots on every step change, and after a failure.
  const [round, setRound] = useState(0);
  const oldPin = useRef("");
  const newPin = useRef("");

  const restart = (message: string) => {
    oldPin.current = "";
    newPin.current = "";
    setError(message);
    setStep("old");
    setRound((n) => n + 1);
  };

  const onComplete = (pin: string) => {
    setError(null);
    if (step === "old") {
      oldPin.current = pin;
      setStep("new");
      setRound((n) => n + 1);
      return;
    }
    if (step === "new") {
      newPin.current = pin;
      setStep("confirm");
      setRound((n) => n + 1);
      return;
    }
    if (pin !== newPin.current) {
      restart(t("changePin.mismatch"));
      return;
    }
    setPin.mutate(
      { oldPin: oldPin.current, newPin: newPin.current },
      {
        onSuccess: () => {
          oldPin.current = "";
          newPin.current = "";
          setDone(true);
        },
        onError: (e) => restart((e as Error).message || t("mobile:auth.pin.saveFailed")),
      },
    );
  };

  if (done) {
    return (
      <Screen testID="screen-change-pin">
        <Header title={t("changePin.title")} backTo={t("security.title")} />
        <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 32, gap: 24 }}>
          <Heading testID="change-pin-done" title={t("changePin.updated")} />
        </Body>
        <BottomAction>
          <View>
            <Button testID="btn-change-pin-done" full variant="primary" onPress={() => router.back()}>
              {t("changePin.doneButton")}
            </Button>
          </View>
        </BottomAction>
      </Screen>
    );
  }

  return (
    <PinEntryPage
      testID="screen-change-pin"
      title={t("changePin.title")}
      backTo={t("security.title")}
      heading={
        step === "old"
          ? t("changePin.headingOld")
          : step === "new"
            ? t("changePin.headingNew")
            : t("changePin.headingConfirm")
      }
      subtitle={
        step === "old"
          ? t("changePin.hintOld")
          : step === "new"
            ? t("changePin.hintNew")
            : t("changePin.hintConfirm")
      }
      error={error}
      errorTestID="change-pin-error"
      busy={setPin.isPending}
      busyLabel={t("auth:pinCreate.saving")}
      resetKey={`${step}-${round}`}
      onComplete={onComplete}
    />
  );
}
