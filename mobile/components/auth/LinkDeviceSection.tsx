import { useEffect, useRef, useState } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { useObserver } from "mobx-react-lite";
import { Button, Field } from "../ui";
import { QrCode } from "../QrCode";
import { semantic, type as ty, fonts } from "../../theme/tokens";
import { upper } from "../../i18n";
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
 * "Link a new device" on mobile (#1207) — the twin of desktop's
 * LinkDeviceSection: PIN (verified in Rust) → QR → wait (one awaited Rust call
 * per state, no polling) → Approve only a request whose link tag Rust has
 * verified. Strings are the shared `settings:linkDevice.*` catalogue.
 */

type Phase =
  | { kind: "idle" }
  | { kind: "pin" }
  | { kind: "showing"; handle: DeviceLinkHandle; status: DeviceLinkStatus }
  | { kind: "linked"; name: string | null }
  | { kind: "error"; message: string };

const body = { fontFamily: ty.body.fontFamily, fontSize: 13, color: semantic.mute } as const;

export function LinkDeviceSection() {
  const { t } = useTranslation("settings");
  const userId = useObserver(() => appStore.currentUser?.id ?? null);
  const create = useCreateDeviceLink();
  const reject = useRejectEnrollment();
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [pin, setPin] = useState("");
  const [pinError, setPinError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
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
        setPhase({ kind: "showing", handle, status: next });
        if (next.state === "open" || next.state === "claimed") {
          follow(handle, next.state);
        }
      } catch (e) {
        if (generation.current === mine) {
          setPhase({ kind: "error", message: (e as Error).message || t("linkDevice.failed") });
        }
      }
    })();
  };

  const start = () => {
    setPinError(null);
    create.mutate(pin, {
      onSuccess: (handle) => {
        setPin("");
        setPhase({ kind: "showing", handle, status: { state: "open" } });
        follow(handle, "open");
      },
      onError: (e) => setPinError((e as Error).message || t("linkDevice.pinFailed")),
    });
  };

  const stop = async (handle?: DeviceLinkHandle) => {
    generation.current += 1;
    if (handle) {
      await cancelDeviceLink(handle.link_id).catch(() => undefined);
    }
    setPin("");
    setPinError(null);
    setPhase({ kind: "idle" });
  };

  const approve = async (handle: DeviceLinkHandle, name: string | null) => {
    if (!userId) {
      return;
    }
    setBusy(true);
    try {
      await approveDeviceLink(userId, handle.link_id);
      generation.current += 1;
      setPhase({ kind: "linked", name });
    } catch (e) {
      setPhase({ kind: "error", message: (e as Error).message || t("linkDevice.failed") });
    } finally {
      setBusy(false);
    }
  };

  return (
    <View testID="link-device-section" style={{ paddingHorizontal: 18, paddingVertical: 12, gap: 12 }}>
      <Text style={body}>{t("linkDevice.description")}</Text>

      {phase.kind === "idle" ? (
        <Button testID="btn-link-device" onPress={() => setPhase({ kind: "pin" })}>
          {upper(t("linkDevice.start"))}
        </Button>
      ) : null}

      {phase.kind === "pin" ? (
        <View style={{ gap: 10 }}>
          <Text style={[ty.label]}>{upper(t("linkDevice.pinLabel"))}</Text>
          <Field
            testID="input-link-device-pin"
            accessibilityLabel={t("linkDevice.pinLabel")}
            value={pin}
            onChangeText={setPin}
            secureTextEntry
            keyboardType="number-pad"
          />
          {pinError ? <Text style={[body, { color: semantic.danger }]}>{pinError}</Text> : null}
          <Button testID="btn-link-device-show" variant="primary" disabled={pin.length === 0 || create.isPending} onPress={start}>
            {upper(t("linkDevice.showCode"))}
          </Button>
          <Button testID="btn-link-device-cancel" variant="subtle" onPress={() => stop()}>
            {upper(t("common:actions.cancel"))}
          </Button>
        </View>
      ) : null}

      {phase.kind === "showing" ? (
        <Showing
          handle={phase.handle}
          status={phase.status}
          busy={busy}
          onApprove={approve}
          onReject={async (handle, requestId) => {
            setBusy(true);
            reject.mutate(requestId, { onSettled: () => setBusy(false) });
            await stop(handle);
          }}
          onRestart={() => setPhase({ kind: "pin" })}
          onCancel={() => stop(phase.handle)}
        />
      ) : null}

      {phase.kind === "linked" ? (
        <View testID="link-device-done" style={{ gap: 10 }}>
          <Text style={[body, { color: semantic.ink }]}>
            {t("linkDevice.linked", { name: phase.name ?? t("linkDevice.unnamedDevice") })}
          </Text>
          <Button variant="subtle" onPress={() => stop()}>
            {upper(t("linkDevice.done"))}
          </Button>
        </View>
      ) : null}

      {phase.kind === "error" ? (
        <View style={{ gap: 10 }}>
          <Text testID="link-device-error" style={[body, { color: semantic.danger }]}>
            {phase.message}
          </Text>
          <Button variant="subtle" onPress={() => stop()}>
            {upper(t("common:actions.back"))}
          </Button>
        </View>
      ) : null}
    </View>
  );
}

function Showing({
  handle,
  status,
  busy,
  onApprove,
  onReject,
  onRestart,
  onCancel,
}: {
  handle: DeviceLinkHandle;
  status: DeviceLinkStatus;
  busy: boolean;
  onApprove: (handle: DeviceLinkHandle, name: string | null) => void;
  onReject: (handle: DeviceLinkHandle, requestId: string) => void;
  onRestart: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation("settings");
  const [secondsLeft, setSecondsLeft] = useState(() => Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
  // A display clock for the visible countdown; it touches no network.
  useEffect(() => {
    const timer = setInterval(() => {
      setSecondsLeft(Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
    }, 1000);
    return () => clearInterval(timer);
  }, [handle.expires_at]);

  if (status.state === "ready_to_approve") {
    const name = status.device_name ?? t("linkDevice.unnamedDevice");
    return (
      <View testID="link-device-approve-card" style={{ gap: 10, borderWidth: 1, borderColor: semantic.accent, padding: 14 }}>
        <Text style={[body, { color: semantic.ink, fontSize: 15 }]}>{t("linkDevice.wantsToSignIn", { name })}</Text>
        <Text style={body}>{t("linkDevice.approveHint")}</Text>
        <Button testID="btn-link-device-approve" variant="primary" disabled={busy} onPress={() => onApprove(handle, status.device_name)}>
          {upper(t("linkDevice.approve"))}
        </Button>
        <Button testID="btn-link-device-reject" variant="danger" disabled={busy} onPress={() => onReject(handle, status.request_id)}>
          {upper(t("linkDevice.reject"))}
        </Button>
      </View>
    );
  }

  if (status.state === "tampered" || status.state === "expired") {
    return (
      <View testID={`link-device-${status.state}`} style={{ gap: 10 }}>
        <Text style={[body, status.state === "tampered" ? { color: semantic.danger } : null]}>
          {status.state === "tampered" ? t("linkDevice.tampered") : t("linkDevice.expired")}
        </Text>
        <Button onPress={onRestart}>{upper(t("linkDevice.newCode"))}</Button>
        <Button variant="subtle" onPress={onCancel}>
          {upper(t("common:actions.cancel"))}
        </Button>
      </View>
    );
  }

  return (
    <View testID="link-device-showing" style={{ gap: 12, alignItems: "flex-start" }}>
      <QrCode testID="link-device-qr" value={handle.qr_payload} size={220} />
      <Text testID="link-device-status" style={body}>
        {status.state === "claimed"
          ? t("linkDevice.claimed", { name: status.device_name ?? t("linkDevice.unnamedDevice") })
          : secondsLeft > 0
            ? t("linkDevice.scanPrompt", { seconds: secondsLeft })
            : t("linkDevice.expired")}
      </Text>
      <Text style={body}>{t("linkDevice.cantScanHint")}</Text>
      <Text testID="link-device-payload" selectable style={{ fontFamily: fonts.mono400, fontSize: 11, color: semantic.ink }}>
        {handle.qr_payload}
      </Text>
      <Button variant="subtle" onPress={onCancel}>
        {upper(t("common:actions.cancel"))}
      </Button>
    </View>
  );
}
