import { useEffect, useState } from "react";
import { ActivityIndicator, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Body, Button } from "../../components/ui";
import { PinCells, PinKeypad } from "../../components/auth/PinPad";
import { Heading } from "../../components/auth/Heading";
import { AuthError } from "../../components/auth/AuthError";
import { semantic } from "../../theme/tokens";
import {
  useSetPin,
  useUnlock,
  useUnlockState,
} from "../../hooks/queries/useAuth";
import { useFinalizeEnrollment } from "../../hooks/queries/useEnrollment";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";

type Stage = "checking" | "create-first" | "create-confirm" | "unlock";

function AuthPIN() {
  const { t } = useTranslation("auth");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const [pin, setPin] = useState("");
  const [firstPin, setFirstPin] = useState("");
  const [stage, setStage] = useState<Stage>("checking");
  const [error, setError] = useState<string | null>(null);

  const setPinMutation = useSetPin();
  const unlockMutation = useUnlock();
  const unlockState = useUnlockState();
  const finalize = useFinalizeEnrollment();

  useEffect(() => {
    unlockState.mutate(undefined, {
      onSuccess: (snapshot) => {
        // Keep the store's lock flag honest for the auto-lock engine
        // (lib/autolock.tsx): landing here needing an unlock means locked,
        // whether we arrived via auto-lock or a cold start.
        if (snapshot.pin_set && !snapshot.is_unlocked) {
          appStore.setLocked(true);
        }
        setStage(snapshot.pin_set ? "unlock" : "create-first");
      },
      onError: () => {
        // Treat a snapshot-read error as first-time setup; set_pin will
        // re-fail loudly if state is actually inconsistent.
        setStage("create-first");
      },
    });
    // unlockState is a stable mutation ref; intentionally fire only once
    // on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A step label only where there are steps (creating a PIN takes two).
  const stepLabel =
    stage === "create-first"
      ? t("mobile:auth.pin.stepEnter")
      : stage === "create-confirm"
        ? t("mobile:auth.pin.stepConfirm")
        : undefined;

  const headline =
    stage === "unlock"
      ? t("mobile:auth.pin.unlockTitle")
      : t("mobile:auth.pin.createTitle");
  const subtitle =
    stage === "unlock"
      ? t("mobile:auth.pin.unlockIntro")
      : t("mobile:auth.pin.createIntro");

  const onComplete = (entered: string) => {
    setError(null);
    if (stage === "unlock") {
      if (!currentUser) {
        setError(t("mobile:auth.pin.noActiveUser"));
        setStage("checking");
        return;
      }
      unlockMutation.mutate(
        { userId: currentUser.id, pin: entered },
        {
          onSuccess: () => {
            appStore.setLocked(false);
            router.replace("/(auth)/initializing");
          },
          onError: (e) => {
            setError((e as Error).message || t("mobile:auth.pin.invalidPin"));
            setPin("");
          },
        },
      );
      return;
    }
    if (stage === "create-first") {
      setFirstPin(entered);
      setPin("");
      setStage("create-confirm");
      return;
    }
    if (stage === "create-confirm") {
      if (entered !== firstPin) {
        setError(t("pinCreate.mismatch"));
        setFirstPin("");
        setPin("");
        setStage("create-first");
        return;
      }
      setPinMutation.mutate(
        { newPin: entered },
        {
          onSuccess: async () => {
            appStore.setLocked(false);
            // Now that `set_pin` has opened the local DB, finalize this
            // device: publish its cert and external-join the account's
            // groups/DMs. Required after device-linking and Secret-Key
            // recovery, a no-op for a fresh signup — run unconditionally,
            // mirroring desktop's handlePinCreated. Best-effort: a failure
            // here must not strand the user on the PIN screen; the next
            // unlock re-publishes the cert and welcomes still deliver.
            try {
              await finalize.mutateAsync();
            } catch (e) {
              console.error("[pin] finalize_device_enrollment failed:", e);
            }
            // First-device signup has a one-time recovery key stashed in
            // the store by `verify_otp`. Show it before initializing so
            // the user can save it before we drop it from memory.
            const pendingSecretKey =
              appStore.pendingSecretKey;
            if (pendingSecretKey) {
              router.replace("/(auth)/emergency-kit");
            } else {
              router.replace("/(auth)/initializing");
            }
          },
          onError: (e) => {
            setError((e as Error).message || t("mobile:auth.pin.saveFailed"));
            setFirstPin("");
            setPin("");
            setStage("create-first");
          },
        },
      );
    }
  };

  const push = (n: string) => {
    if (pin.length >= 4 || stage === "checking") {
      return;
    }
    const next = pin + n;
    setPin(next);
    if (next.length === 4) {
      setTimeout(() => onComplete(next), 120);
    }
  };

  const busy =
    stage === "checking" ||
    setPinMutation.isPending ||
    unlockMutation.isPending;

  return (
    <Screen testID="screen-auth-pin" centered>
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 32, gap: 24 }}>
        {stage === "checking" ? (
          <ActivityIndicator
            color={semantic.accent}
            accessibilityLabel={t("settings:security.permissionChecking")}
          />
        ) : (
          <Heading step={stepLabel} title={headline} subtitle={subtitle} />
        )}

        <View style={{ gap: 16 }}>
          <PinCells length={pin.length} />
          {error ? <AuthError center message={error} /> : null}
        </View>

        {/* Always offer a way out of this screen — without it, an error
            like "no key material to wrap; sign in again" strands the user
            on the keypad with no path back to sign-in. */}
        <View style={{ alignItems: "center" }}>
          <Button
            testID="btn-pin-signout"
            variant="subtle"
            onPress={() => router.replace("/(auth)/email")}
          >
            {t("enroll.signInAgain")}
          </Button>
        </View>
      </Body>

      <PinKeypad
        onDigit={push}
        onBackspace={() => setPin(pin.slice(0, -1))}
        disabled={busy}
      />
    </Screen>
  );
}

export default observer(AuthPIN);
