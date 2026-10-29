import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Text, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { CameraView, useCameraPermissions } from "expo-camera";
import { useObserver } from "mobx-react-lite";
import { Screen, Header, Button, BottomAction, Field, Body } from "../../components/ui";
import { ErrorText } from "../../components/self/SettingsField";
import { Heading } from "../../components/auth/Heading";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { PinBusy, PinCells, PinKeypad } from "../../components/auth/PinPad";
import { semantic, type as ty, fonts, r } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import { acceptDeviceOffer, cancelDeviceLink, finishDeviceOffer } from "../../hooks/queries";
import { isNewDeviceCode, isSignInCode } from "../../lib/linkCodes";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

/**
 * Scan to sign in a device (#1361). The code carries the new device's
 * ephemeral public key, never a secret; Rust checks the PIN, then wraps the
 * account key only to that exact key, so neither the server nor anyone who
 * photographed the code can substitute their own.
 */

type Step =
  | { kind: "scan" }
  | { kind: "pin"; payload: string }
  | { kind: "linking"; name: string | null }
  | { kind: "linked"; name: string | null }
  | { kind: "failed"; title: string; message: string };

export default function ScanToSignIn() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const router = useRouter();
  // The tab bar's shield (#1550) opens this page from any tab, so back there
  // is plain "Back", not "Back to Security".
  const { from } = useLocalSearchParams<{ from?: string }>();
  const regular = useIsRegular();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const [permission, requestPermission] = useCameraPermissions();
  const [step, setStep] = useState<Step>({ kind: "scan" });
  const [manual, setManual] = useState(false);
  const [code, setCode] = useState("");
  const [scanError, setScanError] = useState<string | null>(null);
  const [pin, setPin] = useState("");
  const [pinError, setPinError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Synchronous twin of `busy`: a second tap in the same frame as the fourth
  // digit must not submit the PIN twice.
  const submitting = useRef(false);
  // The camera reports every frame the code is in view; take it once.
  const taken = useRef(false);
  const generation = useRef(0);
  const linkId = useRef<string | null>(null);

  useEffect(() => {
    return () => {
      generation.current += 1;
      if (linkId.current) {
        void cancelDeviceLink(linkId.current).catch(() => undefined);
      }
    };
  }, []);

  const take = (payload: string) => {
    const text = payload.trim();
    if (taken.current) {
      return;
    }
    if (isSignInCode(text)) {
      setScanError(t("mobile:self.scanDevice.wrongDirection"));
      return;
    }
    if (!isNewDeviceCode(text)) {
      return;
    }
    taken.current = true;
    setScanError(null);
    // A code was read: felt like a toggle, the way a scanner clicks.
    haptic("toggle");
    setStep({ kind: "pin", payload: text });
  };

  const finish = (id: string, name: string | null) => {
    if (!userId) {
      return;
    }
    generation.current += 1;
    const mine = generation.current;
    setStep({ kind: "linking", name });
    void (async () => {
      try {
        const outcome = await finishDeviceOffer(userId, id);
        if (generation.current !== mine) {
          return;
        }
        linkId.current = null;
        if (outcome.state === "linked") {
          haptic("success");
          setStep({ kind: "linked", name: outcome.device_name ?? name });
        } else if (outcome.state === "tampered") {
          setStep({ kind: "failed", title: t("linkDevice.tamperedTitle"), message: t("linkDevice.tampered") });
        } else {
          setStep({
            kind: "failed",
            title: t("linkDevice.expiredTitle"),
            message: t("mobile:self.scanDevice.notFinished"),
          });
        }
      } catch (e) {
        if (generation.current === mine) {
          setStep({ kind: "failed", title: t("linkDevice.failed"), message: (e as Error).message });
        }
      }
    })();
  };

  const submitPin = async (payload: string, value: string) => {
    if (!userId || submitting.current) {
      return;
    }
    submitting.current = true;
    setBusy(true);
    setPinError(null);
    try {
      const accepted = await acceptDeviceOffer(userId, value, payload);
      linkId.current = accepted.link_id;
      finish(accepted.link_id, accepted.device_name);
    } catch (e) {
      const message = (e as Error).message || t("linkDevice.pinFailed");
      // A wrong PIN stays on the keypad; anything else (an expired or used
      // code) means scanning again.
      if (/pin/i.test(message)) {
        setPinError(message);
      } else {
        setStep({ kind: "failed", title: t("linkDevice.failed"), message });
      }
    } finally {
      setPin("");
      setBusy(false);
      submitting.current = false;
    }
  };

  const restart = () => {
    generation.current += 1;
    taken.current = false;
    linkId.current = null;
    setCode("");
    setPin("");
    setPinError(null);
    setScanError(null);
    setStep({ kind: "scan" });
  };

  const crumb = (
    <Header
      title={t("mobile:self.scanDevice.heading")}
      backTo={from === "tab" ? undefined : t("settings:security.title")}
    />
  );

  if (step.kind === "pin") {
    const pushDigit = (d: string) => {
      if (busy || pin.length >= 4) {
        return;
      }
      const next = pin + d;
      setPin(next);
      if (next.length === 4) {
        void submitPin(step.payload, next);
      }
    };
    return (
      <Screen testID="screen-link-device">
        {crumb}
        <View style={regular ? { flex: 1, justifyContent: "center", gap: 32 } : { flex: 1 }}>
          <View style={{ flex: regular ? 0 : 1, paddingHorizontal: 24, paddingTop: regular ? 0 : 32, gap: 32 }}>
            <Heading
              testID="scan-device-confirm"
              title={t("mobile:self.scanDevice.confirmTitle")}
              subtitle={t("mobile:self.scanDevice.confirmBody")}
            />
            <View style={{ gap: 14 }}>
              <PinCells length={pin.length} />
              {busy ? (
                <PinBusy label={t("security.verifying")} />
              ) : pinError ? (
                <View style={{ alignSelf: "center" }}>
                  <ErrorText testID="link-device-pin-error">{pinError}</ErrorText>
                </View>
              ) : null}
            </View>
          </View>
          <PinKeypad onDigit={pushDigit} onBackspace={() => setPin(pin.slice(0, -1))} disabled={busy} />
        </View>
      </Screen>
    );
  }

  if (step.kind === "linking" || step.kind === "linked") {
    const name = step.name ?? t("linkDevice.unnamedDevice");
    const done = step.kind === "linked";
    return (
      <Screen testID="screen-link-device">
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32, gap: 32 }}>
          <Heading
            testID={done ? "link-device-done" : "link-device-linking"}
            title={done ? t("linkDevice.linked", { name }) : t("mobile:self.scanDevice.linking", { name })}
            subtitle={done ? undefined : t("mobile:self.scanDevice.linkingBody")}
          />
          {done ? null : <ActivityIndicator color={semantic.accent} />}
        </View>
        {done ? (
          <BottomAction>
            <Button testID="btn-link-device-done" variant="primary" full onPress={() => router.back()}>
              {t("linkDevice.done")}
            </Button>
          </BottomAction>
        ) : null}
      </Screen>
    );
  }

  if (step.kind === "failed") {
    return (
      <Screen testID="screen-link-device">
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32 }}>
          <Heading testID="link-device-failed" title={step.title} subtitle={step.message} />
        </View>
        <BottomAction>
          <Button testID="btn-scan-again" variant="primary" full onPress={restart}>
            {t("mobile:self.scanDevice.scanAgain")}
          </Button>
          <Button variant="subtle" full onPress={() => router.back()}>
            {t("common:actions.cancel")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  return (
    <Screen testID="screen-link-device">
      {crumb}
      <Body contentContainerStyle={{ paddingHorizontal: 24, paddingTop: 8, gap: 24 }}>
        <Heading
          title={t("mobile:self.scanDevice.scanTitle")}
          subtitle={manual ? t("mobile:self.scanDevice.codeIntro") : t("mobile:self.scanDevice.scanIntro")}
        />
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
              testID="scan-device-camera"
              style={{ flex: 1 }}
              facing="back"
              barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
              onBarcodeScanned={({ data }) => take(data)}
            />
          </View>
        ) : null}
        {!manual && permission && !permission.granted ? (
          <Button testID="btn-scan-allow-camera" variant="primary" full onPress={() => void requestPermission()}>
            {t("auth:link.allowCamera")}
          </Button>
        ) : null}
        {manual ? (
          <View style={{ gap: 8 }}>
            <Text style={ty.section}>{t("auth:link.codeLabel")}</Text>
            <Field
              testID="input-scan-code"
              accessibilityLabel={t("auth:link.codeLabel")}
              value={code}
              onChangeText={(next) => {
                setScanError(null);
                setCode(next);
              }}
              autoCorrect={false}
              placeholder="pollis-link:v2:…"
              style={{ fontFamily: fonts.mono400 }}
            />
          </View>
        ) : null}
        {scanError ? <ErrorText testID="scan-device-error">{scanError}</ErrorText> : null}
        <Text style={[ty.meta, { color: semantic.dim }]}>{t("mobile:self.scanDevice.warning")}</Text>
      </Body>
      <BottomAction>
        {manual && code.trim() ? (
          <Button testID="btn-scan-code-continue" variant="primary" full onPress={() => take(code)}>
            {t("mobile:self.scanDevice.continue")}
          </Button>
        ) : null}
        <Button
          testID="btn-scan-toggle-manual"
          variant="secondary"
          full
          onPress={() => {
            setManual((m) => !m);
            setScanError(null);
          }}
        >
          {manual ? t("auth:link.scanInstead") : t("auth:link.enterCode")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
