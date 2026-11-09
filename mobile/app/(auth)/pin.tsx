import { useEffect, useState } from "react";
import { Platform, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Body, Button } from "../../components/ui";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { PinCells, PinKeypad } from "../../components/auth/PinPad";
import { LaunchScreen } from "../../components/LaunchScreen";
import { Heading } from "../../components/auth/Heading";
import { Icon } from "../../components/icons";
import { semantic, type as ty } from "../../theme/tokens";
import { AuthError } from "../../components/auth/AuthError";
import {
  useSetPin,
  useUnlock,
  useUnlockState,
} from "../../hooks/queries/useAuth";
import { useFinalizeEnrollment } from "../../hooks/queries/useEnrollment";
import { markLinkOffer } from "../../hooks/queries/useHistoryTransfer";
import { useSubmitLock } from "../../hooks/useSubmitLock";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { useThemeRerender } from "../../components/theme";
import {
  afterPinUnlock,
  pinScreenView,
  pinSubmitIsSlow,
  type PinStage as Stage,
} from "../../lib/pinStage";
import {
  biometricMethod,
  deviceUnlockSupported,
  enableDeviceUnlock,
  markNeedsRearm,
  readDeviceKey,
  takeNeedsRearm,
  unlockWithDeviceKey,
  wasOffered,
} from "../../lib/deviceUnlock";
import { haptic } from "../../lib/haptics";

function AuthPIN() {
  useThemeRerender();
  const { t } = useTranslation("auth");
  const router = useRouter();
  const currentUser = appStore.currentUser;
  const [pin, setPin] = useState("");
  const [firstPin, setFirstPin] = useState("");
  const [stage, setStage] = useState<Stage>("checking");
  const [error, setError] = useState<string | null>(null);
  // Biometric unlock (#1685) is on for this user, per core.
  const [biometricOn, setBiometricOn] = useState(false);
  // Shown over the keypad when the biometric key was invalidated (biometrics re-enrolled).
  const [notice, setNotice] = useState<string | null>(null);

  const setPinMutation = useSetPin();
  const unlockMutation = useUnlock();
  const unlockState = useUnlockState();
  const finalize = useFinalizeEnrollment();
  // Held from the fourth digit: Argon2id and opening the encrypted DB take a moment.
  const submit = useSubmitLock();

  useEffect(() => {
    unlockState.mutate(undefined, {
      onSuccess: (snapshot) => {
        // Keep the store's lock flag honest for the auto-lock engine.
        if (snapshot.pin_set && !snapshot.is_unlocked) {
          appStore.setLocked(true);
        }
        if (!snapshot.pin_set) {
          setStage("create-first");
          return;
        }
        const on = snapshot.device_unlock === true;
        setBiometricOn(on);
        // Biometrics first when on; the keypad is the fallback for everything else.
        if (on && !snapshot.is_unlocked && deviceUnlockSupported()) {
          void tryBiometric();
          return;
        }
        setStage("unlock");
      },
      onError: () => {
        // Treat a read error as first-time setup; set_pin re-fails loudly if wrong.
        setStage("create-first");
      },
    });
    // unlockState is a stable mutation ref; intentionally fire only once
    // on mount.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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

  // One system prompt; a key unlocks through core's own unlock path. Anything else is the keypad:
  // a cancel or failure as it was, a missing key (re-enrolled) with a note, re-armed after the PIN.
  const tryBiometric = async () => {
    const user = appStore.currentUser;
    if (!user) {
      setStage("unlock");
      return;
    }
    setError(null);
    setStage("biometric");
    const read = await readDeviceKey(user.id);
    if (read.kind === "key") {
      try {
        await unlockWithDeviceKey(user.id, read.key);
        haptic("success");
        appStore.setLocked(false);
        router.replace("/(auth)/initializing");
        return;
      } catch (e) {
        const message = (e as Error).message ?? "";
        if (/locked out/i.test(message)) {
          setError(message);
        } else {
          // The key no longer matches what core holds: the PIN re-arms it.
          markNeedsRearm(user.id);
        }
      }
    } else if (read.kind === "invalidated") {
      markNeedsRearm(user.id);
      setNotice(t("mobile:biometric.changed", { method: biometricMethod() }));
    }
    setStage("unlock");
  };

  // After a PIN unlock: re-arm biometrics if their key was lost, or show the one-time offer.
  const continueAfterPin = async (userId: string) => {
    const next = afterPinUnlock({
      needsRearm: takeNeedsRearm(userId),
      supported: deviceUnlockSupported(),
      enabled: biometricOn,
      offered: await wasOffered(userId),
    });
    if (next === "rearm") {
      try {
        await enableDeviceUnlock(userId, { readBack: false });
      } catch (e) {
        console.warn("[pin] re-arming biometric unlock failed:", e);
      }
    }
    if (next === "offer") {
      router.replace("/(auth)/biometric-offer");
      return;
    }
    router.replace("/(auth)/initializing");
  };

  const onComplete = (entered: string) => {
    setError(null);
    if (stage === "unlock") {
      if (!currentUser) {
        submit.end();
        setError(t("mobile:auth.pin.noActiveUser"));
        setStage("checking");
        return;
      }
      unlockMutation.mutate(
        { userId: currentUser.id, pin: entered },
        {
          onSuccess: () => {
            appStore.setLocked(false);
            void continueAfterPin(currentUser.id);
          },
          onError: (e) => {
            setError((e as Error).message || t("mobile:auth.pin.invalidPin"));
            setPin("");
            submit.end();
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
        submit.end();
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
            // Finalize this device unconditionally (a no-op for a fresh signup). Best-effort: never strand the user here.
            let finalized = false;
            try {
              await finalize.mutateAsync();
              finalized = true;
            } catch (e) {
              console.error("[pin] finalize_device_enrollment failed:", e);
            }
            // First-device signup: show the one-time recovery key before initializing.
            const pendingSecretKey =
              appStore.pendingSecretKey;
            // #1735: a device added to an existing account is offered its history after setup.
            if (finalized && !pendingSecretKey) {
              markLinkOffer();
            }
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
            submit.end();
          },
        },
      );
    }
  };

  const push = (n: string) => {
    if (pin.length >= 4 || stage === "checking" || submit.busy) {
      return;
    }
    const next = pin + n;
    setPin(next);
    if (next.length === 4) {
      // Go busy on this very tap, so the loader replaces the keypad before the slow call starts.
      // A mismatched confirm makes no call, so it stays on the pad with its error.
      const mismatch = stage === "create-confirm" && next !== firstPin;
      if (pinSubmitIsSlow(stage) && !mismatch) {
        if (!submit.begin()) {
          return;
        }
        setError(null);
      }
      setTimeout(() => onComplete(next), 120);
    }
  };

  const regular = useIsRegular();
  const working = submit.busy;

  // A PIN is in and the slow call runs: the launch loader replaces the whole pad (no frozen keypad).
  const view = pinScreenView(stage, working, !!error);
  if (view !== "keypad") {
    return (
      <View testID="screen-auth-pin" collapsable={false} style={{ flex: 1 }}>
        <LaunchScreen
          testID={view === "submitting" ? "pin-busy" : view === "biometric" ? "pin-biometric" : "pin-checking"}
          label={
            view === "checking"
              ? t("settings:security.permissionChecking")
              : stage === "unlock" || stage === "biometric"
                ? t("pinEntry.unlocking")
                : t("pinCreate.saving")
          }
        />
      </View>
    );
  }

  const busy =
    stage === "checking" ||
    working ||
    setPinMutation.isPending ||
    unlockMutation.isPending;

  const pinContent = (
    <>
      {stage === "checking" ? null : (
        <Heading step={stepLabel} title={headline} subtitle={subtitle} />
      )}

      <View style={{ gap: 16 }}>
        <PinCells length={pin.length} />
        {error ? <AuthError center testID="pin-error" message={error} /> : null}
        {notice && !error ? (
          <Text testID="pin-biometric-notice" style={[ty.secondary, { textAlign: "center" }]}>
            {notice}
          </Text>
        ) : null}
      </View>

      {stage === "unlock" && biometricOn && deviceUnlockSupported() ? (
        <View style={{ alignItems: "center" }}>
          <Button
            testID="btn-pin-biometric"
            variant="subtle"
            disabled={working}
            icon={
              Platform.OS === "ios" ? (
                <Icon.faceId size={18} color={semantic.text} />
              ) : (
                <Icon.fingerprint size={18} color={semantic.text} />
              )
            }
            onPress={() => void tryBiometric()}
          >
            {t("mobile:biometric.useBiometric", { method: biometricMethod() })}
          </Button>
        </View>
      ) : null}

      {/* Always offer a way out, or an error strands the user on the keypad. */}
      <View style={{ alignItems: "center" }}>
        <Button
          testID="btn-pin-signout"
          variant="subtle"
          disabled={working}
          onPress={() => router.replace("/(auth)/email")}
        >
          {t("enroll.signInAgain")}
        </Button>
      </View>
    </>
  );
  const keypad = (
    <PinKeypad
      onDigit={push}
      onBackspace={() => setPin(pin.slice(0, -1))}
      disabled={busy}
    />
  );

  // The keypad is the only input: no keyboard padding can ever apply (#1642).
  return (
    <Screen testID="screen-auth-pin" centered avoidKeyboard={false}>
      {regular ? (
        // iPad: heading, dots and keypad as one centred block.
        <View style={{ flex: 1, justifyContent: "center", gap: 32 }}>
          <View style={{ paddingHorizontal: 24, gap: 24 }}>{pinContent}</View>
          {keypad}
        </View>
      ) : (
        <>
          <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 32, gap: 24 }}>
            {pinContent}
          </Body>
          {keypad}
        </>
      )}
    </Screen>
  );
}

export default observer(AuthPIN);
