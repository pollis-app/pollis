import { useEffect, useRef, useState } from "react";
import { ActivityIndicator, Pressable, Text, View } from "react-native";
import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import * as Clipboard from "expo-clipboard";
import { useObserver } from "mobx-react-lite";
import { Screen, Header, Button, BottomAction } from "../../components/ui";
import { Icon } from "../../components/icons";
import { ErrorText } from "../../components/self/SettingsField";
import { Heading } from "../../components/auth/Heading";
import { useIsRegular } from "../../hooks/useLayoutClass";
import { PinCells, PinKeypad } from "../../components/auth/PinPad";
import { QrCode } from "../../components/QrCode";
import { SegmentedControl } from "../../components/self/SegmentedControl";
import { semantic, type as ty, fonts, r, space } from "../../theme/tokens";
import { appStore } from "../../stores/appStore";
import {
  approveDeviceLink,
  awaitDeviceLink,
  cancelDeviceLink,
  useCreateDeviceLink,
  useRejectEnrollment,
  type DeviceLinkHandle,
  type DeviceLinkStatus,
} from "../../hooks/queries";

/**
 * Link a new device (#1207), one step per screen, laid out like sign-in:
 * PIN → the code (QR or copyable text) → waiting → Approve → done. The PIN is
 * verified in Rust; the key moves only on Approve of a request whose link tag
 * Rust verified. Waiting is one awaited Rust call per state, never a poll.
 */

type Step =
  | { kind: "pin" }
  | { kind: "code"; handle: DeviceLinkHandle; status: DeviceLinkStatus }
  | { kind: "linked"; name: string };

export default function LinkDevice() {
  const { t } = useTranslation("settings");
  const router = useRouter();
  const regular = useIsRegular();
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const create = useCreateDeviceLink();
  const reject = useRejectEnrollment();
  const [step, setStep] = useState<Step>({ kind: "pin" });
  const [pin, setPin] = useState("");
  const [pinError, setPinError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);

  useEffect(() => {
    return () => {
      generation.current += 1;
    };
  }, []);

  const follow = (handle: DeviceLinkHandle, since: DeviceLinkStatus["state"]) => {
    if (!userId) {
      return;
    }
    generation.current += 1;
    const mine = generation.current;
    void (async () => {
      try {
        const next = await awaitDeviceLink(userId, handle.link_id, since);
        if (generation.current !== mine) {
          return;
        }
        setStep({ kind: "code", handle, status: next });
        if (next.state === "open" || next.state === "claimed") {
          follow(handle, next.state);
        }
      } catch (e) {
        if (generation.current === mine) {
          setError((e as Error).message || t("linkDevice.failed"));
        }
      }
    })();
  };

  const submitPin = (value: string) => {
    setPinError(null);
    create.mutate(value, {
      onSuccess: (handle) => {
        setPin("");
        setStep({ kind: "code", handle, status: { state: "open" } });
        follow(handle, "open");
      },
      onError: (e) => {
        setPin("");
        setPinError((e as Error).message || t("linkDevice.pinFailed"));
      },
    });
  };

  const pushDigit = (d: string) => {
    if (create.isPending || pin.length >= 4) {
      return;
    }
    const next = pin + d;
    setPin(next);
    if (next.length === 4) {
      submitPin(next);
    }
  };

  const leave = async () => {
    generation.current += 1;
    if (step.kind === "code") {
      await cancelDeviceLink(step.handle.link_id).catch(() => undefined);
    }
    router.back();
  };

  const restart = () => {
    generation.current += 1;
    setError(null);
    setPin("");
    setStep({ kind: "pin" });
  };

  const approve = async (handle: DeviceLinkHandle, name: string) => {
    if (!userId) {
      return;
    }
    setBusy(true);
    try {
      await approveDeviceLink(userId, handle.link_id);
      generation.current += 1;
      setStep({ kind: "linked", name });
    } catch (e) {
      setError((e as Error).message || t("linkDevice.failed"));
    } finally {
      setBusy(false);
    }
  };

  const crumb = (
    <Header title={t("linkDevice.heading")} backTo={t("settings:security.title")} onBack={() => void leave()} />
  );

  if (step.kind === "pin") {
    return (
      <Screen testID="screen-link-device" centered>
        {crumb}
        {/* iPad: the PIN block (heading, dots, keypad) centred vertically
            rather than the keypad pinned to the bottom edge. */}
        <View style={regular ? { flex: 1, justifyContent: "center", gap: 32 } : { flex: 1 }}>
          <View style={{ flex: regular ? 0 : 1, paddingHorizontal: 24, paddingTop: regular ? 0 : 32, gap: 32 }}>
            <Heading title={t("linkDevice.pinTitle")} subtitle={t("linkDevice.pinSubtitle")} />
            <View style={{ gap: 14 }}>
              <PinCells length={pin.length} />
              {pinError ? (
                <View style={{ alignSelf: "center" }}>
                  <ErrorText testID="link-device-pin-error">{pinError}</ErrorText>
                </View>
              ) : null}
            </View>
          </View>
          <PinKeypad onDigit={pushDigit} onBackspace={() => setPin(pin.slice(0, -1))} disabled={create.isPending} />
        </View>
      </Screen>
    );
  }

  if (step.kind === "linked") {
    return (
      <Screen testID="screen-link-device" centered>
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32 }}>
          <Heading testID="link-device-done" title={t("linkDevice.linked", { name: step.name })} />
        </View>
        <BottomAction>
          <Button testID="btn-link-device-done" variant="primary" full onPress={() => router.back()}>
            {t("linkDevice.done")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  const { handle, status } = step;
  const name = (status.state === "claimed" || status.state === "ready_to_approve" ? status.device_name : null) ?? t("linkDevice.unnamedDevice");

  if (error || status.state === "tampered" || status.state === "expired") {
    const tampered = status.state === "tampered";
    return (
      <Screen testID="screen-link-device" centered>
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32 }}>
          <Heading
            testID={tampered ? "link-device-tampered" : "link-device-expired"}
            title={tampered ? t("linkDevice.tamperedTitle") : error ? t("linkDevice.failed") : t("linkDevice.expiredTitle")}
            subtitle={tampered ? t("linkDevice.tampered") : error ?? t("linkDevice.expired")}
          />
        </View>
        <BottomAction>
          <Button variant="primary" full onPress={restart}>
            {t("linkDevice.newCode")}
          </Button>
          <Button variant="subtle" full onPress={leave}>
            {t("common:actions.cancel")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  if (status.state === "ready_to_approve") {
    return (
      <Screen testID="screen-link-device" centered>
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32 }}>
          <Heading
            testID="link-device-approve-card"
            title={t("linkDevice.wantsToSignIn", { name })}
            subtitle={t("linkDevice.approveHint")}
          />
        </View>
        <BottomAction>
          <Button testID="btn-link-device-approve" variant="primary" full disabled={busy} onPress={() => approve(handle, name)}>
            {t("linkDevice.approve")}
          </Button>
          <Button
            testID="btn-link-device-reject"
            variant="secondary"
            full
            disabled={busy}
            onPress={() => {
              reject.mutate(status.request_id);
              void leave();
            }}
          >
            {t("linkDevice.reject")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  if (status.state === "claimed") {
    return (
      <Screen testID="screen-link-device" centered>
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32, gap: 32 }}>
          <Heading testID="link-device-claimed" title={t("linkDevice.claimedTitle")} subtitle={t("linkDevice.claimed", { name })} />
          <ActivityIndicator color={semantic.accent} />
        </View>
        <BottomAction>
          <Button variant="subtle" full onPress={leave}>
            {t("common:actions.cancel")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  return <ShowCode handle={handle} crumb={crumb} onCancel={leave} />;
}

// Lets a long unbroken token wrap at any character (see ShowCode).
function breakAnywhere(text: string): string {
  return Array.from(text).join("\u200B");
}

/** The code, as a QR (default) or as text with one-tap copy. */
function ShowCode({ handle, crumb, onCancel }: { handle: DeviceLinkHandle; crumb: React.ReactNode; onCancel: () => void }) {
  const { t } = useTranslation("settings");
  const [mode, setMode] = useState<"qr" | "code">("qr");
  const [copied, setCopied] = useState(false);
  const [secondsLeft, setSecondsLeft] = useState(() => Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
  // A display clock for the countdown; it touches no network.
  useEffect(() => {
    const timer = setInterval(() => {
      setSecondsLeft(Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
    }, 1000);
    return () => clearInterval(timer);
  }, [handle.expires_at]);

  const copy = async () => {
    await Clipboard.setStringAsync(handle.qr_payload);
    setCopied(true);
  };

  return (
    <Screen testID="screen-link-device" centered>
      {crumb}
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32, gap: 28 }}>
        <Heading
          title={mode === "qr" ? t("linkDevice.showTitle") : t("mobile:self.linkDevice.codeTitle")}
          subtitle={mode === "qr" ? t("linkDevice.showSubtitle") : t("linkDevice.codeSubtitle")}
        />
        <SegmentedControl
          value={mode}
          onChange={setMode}
          segments={[
            { key: "qr", label: t("linkDevice.tabQr"), testID: "chip-link-qr" },
            { key: "code", label: t("linkDevice.tabCode"), testID: "chip-link-code" },
          ]}
        />

        {mode === "qr" ? (
          <View testID="link-device-showing" style={{ alignItems: "center" }}>
            <QrCode testID="link-device-qr" value={handle.qr_payload} size={232} frame={space.lg} radius={r.lg} />
          </View>
        ) : (
          <Pressable
            testID="link-device-copy"
            accessibilityRole="button"
            accessibilityLabel={t("linkDevice.copyHint")}
            onPress={copy}
            style={({ pressed }) => ({
              backgroundColor: pressed ? semantic.high : semantic.raised,
              borderRadius: r.lg,
              padding: space.xxl,
              gap: space.lg,
            })}
          >
            {/* A zero-width space between every character lets the line
                break anywhere, so the code fills each line evenly instead of
                breaking at its hyphens. Copy uses the raw payload. */}
            <Text testID="link-device-payload" style={{ fontFamily: fonts.mono400, fontSize: 15, lineHeight: 22, color: semantic.text }}>
              {breakAnywhere(handle.qr_payload)}
            </Text>
            {/* The whole card is the copy target; the hint reads as its
                action, in accent. */}
            <View style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}>
              {copied ? <Icon.check size={16} color={semantic.accent} /> : <Icon.copy size={16} color={semantic.accent} />}
              <Text style={[ty.section, { color: semantic.accent }]}>
                {copied ? t("linkDevice.copied") : t("linkDevice.copyHint")}
              </Text>
            </View>
          </Pressable>
        )}

        <Text testID="link-device-status" style={[ty.secondary, { textAlign: "center" }]}>
          {secondsLeft > 0
            ? t("linkDevice.expiresIn", { time: `${Math.floor(secondsLeft / 60)}:${String(secondsLeft % 60).padStart(2, "0")}` })
            : t("linkDevice.expiredTitle")}
        </Text>
      </View>
      <BottomAction>
        <Button variant="subtle" full onPress={onCancel}>
          {t("common:actions.cancel")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
