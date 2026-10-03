import { useEffect, useState } from "react";
import { View, Text, Pressable } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { Screen, Crumb } from "../../components/ui";
import { Icon } from "../../components/icons";
import { PinCells, PinKeypad } from "../../components/auth/PinPad";
import { semantic, type as ty } from "../../theme/tokens";
import {
  useSetPin,
  useUnlock,
  useUnlockState,
} from "../../hooks/queries/useAuth";
import { useFinalizeEnrollment } from "../../hooks/queries/useEnrollment";
import { appStore } from "../../stores/appStore";
import { observer } from "mobx-react-lite";
import { upper } from "../../i18n";

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

  const stageLabel = (() => {
    switch (stage) {
      case "checking":
        return upper(t("settings:security.permissionChecking"));
      case "create-first":
        return upper(t("mobile:auth.pin.stepEnter"));
      case "create-confirm":
        return upper(t("mobile:auth.pin.stepConfirm"));
      case "unlock":
        return upper(t("mobile:auth.pin.unlockPrompt"));
    }
  })();

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
      <Crumb
        segs={[
          { label: upper(t("mobile:auth.crumb.auth")) },
          {
            label:
              stage === "unlock"
                ? t("mobile:auth.crumb.unlockDevice")
                : t("mobile:auth.crumb.setDevicePin"),
            leaf: true,
          },
        ]}
      />
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 24, gap: 18 }}>
        <View style={{ gap: 8 }}>
          <Text style={[ty.h1, { color: semantic.ink }]}>{headline}</Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              lineHeight: 19,
              color: semantic.mute,
            }}
          >
            {subtitle}
          </Text>
        </View>

        <View style={{ paddingVertical: 14 }}>
          <PinCells length={pin.length} />
          <Text
            style={[ty.label, { textAlign: "center", marginTop: 14 }]}
          >
            {stageLabel}
          </Text>
          {error ? (
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 12,
                color: semantic.danger,
                textAlign: "center",
                marginTop: 8,
              }}
            >
              {error}
            </Text>
          ) : null}

          {/* Always offer a way out of this screen — without it, an error
              like "no key material to wrap; sign in again" strands the user
              on the keypad with no path back to sign-in. */}
          <Pressable
            onPress={() => router.replace("/(auth)/email")}
            testID="btn-pin-signout"
            accessibilityRole="button"
            accessibilityLabel={t("enroll.signInAgain")}
            style={{
              flexDirection: "row",
              alignItems: "center",
              alignSelf: "flex-start",
              gap: 8,
              marginTop: 40,
            }}
          >
            <Icon.back color={semantic.ink} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 16,
                color: semantic.ink,
              }}
            >
              {t("enroll.signInAgain")}
            </Text>
          </Pressable>
        </View>
      </View>

      <PinKeypad
        onDigit={push}
        onBackspace={() => setPin(pin.slice(0, -1))}
        disabled={busy}
      />
    </Screen>
  );
}

export default observer(AuthPIN);
