import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { QRCodeSVG } from "qrcode.react";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import * as api from "../../services/api";
import { errorMessage } from "../../utils/errorMessage";

/**
 * "Link a new device" (#1207): show a QR a phone scans to sign in without the
 * email code. The PIN is required first — verified in Rust, so this screen
 * cannot skip it — and the key only moves when the user taps Approve on a
 * request whose link tag Rust has verified. docs/qr-device-link-design.md.
 *
 * No renderer polling: each wait is one awaited `awaitDeviceLink`, whose
 * backoff and deadline live in Rust. The only timer here is the visible
 * countdown, which touches no network.
 */

type Phase =
  | { kind: "idle" }
  | { kind: "pin" }
  | { kind: "showing"; handle: api.DeviceLinkHandle; status: api.DeviceLinkStatus }
  | { kind: "linked"; deviceName: string | null }
  | { kind: "error"; message: string };

// Scanner-friendly: dark modules on light, with a quiet zone, whatever the
// app theme. An inverted (light-on-dark) code fails on some camera decoders.
const QR_DARK = "#000000";
const QR_LIGHT = "#ffffff";

export const LinkDeviceSection: React.FC<{ userId: string }> = ({ userId }) => {
  const { t } = useTranslation("settings");
  const [phase, setPhase] = useState<Phase>({ kind: "idle" });
  const [pin, setPin] = useState("");
  const [busy, setBusy] = useState(false);
  const [pinError, setPinError] = useState<string | null>(null);
  // Bumped whenever a wait is abandoned; a late answer from a superseded wait
  // is ignored (the promise itself cannot be cancelled).
  const generation = useRef(0);

  useEffect(() => {
    return () => {
      generation.current += 1;
    };
  }, []);

  const follow = (handle: api.DeviceLinkHandle, since: api.DeviceLinkStatus["state"]) => {
    generation.current += 1;
    const mine = generation.current;
    void (async () => {
      try {
        const next = await api.awaitDeviceLink(userId, handle.link_id, since);
        if (generation.current !== mine) {
          return;
        }
        setPhase({ kind: "showing", handle, status: next });
        if (next.state === "open" || next.state === "claimed") {
          follow(handle, next.state);
        }
      } catch (err) {
        if (generation.current === mine) {
          setPhase({ kind: "error", message: errorMessage(err) || t("linkDevice.failed") });
        }
      }
    })();
  };

  const start = async () => {
    if (busy || pin.length === 0) {
      return;
    }
    setBusy(true);
    setPinError(null);
    try {
      const handle = await api.createDeviceLink(userId, pin);
      setPin("");
      const status: api.DeviceLinkStatus = { state: "open" };
      setPhase({ kind: "showing", handle, status });
      follow(handle, "open");
    } catch (err) {
      setPinError(errorMessage(err) || t("linkDevice.pinFailed"));
    } finally {
      setBusy(false);
    }
  };

  const stop = async (handle?: api.DeviceLinkHandle) => {
    generation.current += 1;
    if (handle) {
      await api.cancelDeviceLink(handle.link_id).catch(() => undefined);
    }
    setPin("");
    setPinError(null);
    setPhase({ kind: "idle" });
  };

  const approve = async (handle: api.DeviceLinkHandle, deviceName: string | null) => {
    setBusy(true);
    try {
      await api.approveDeviceLink(userId, handle.link_id);
      generation.current += 1;
      setPhase({ kind: "linked", deviceName });
    } catch (err) {
      setPhase({ kind: "error", message: errorMessage(err) || t("linkDevice.failed") });
    } finally {
      setBusy(false);
    }
  };

  const reject = async (handle: api.DeviceLinkHandle, requestId: string) => {
    setBusy(true);
    try {
      await api.rejectDeviceEnrollment(requestId);
    } finally {
      setBusy(false);
      await stop(handle);
    }
  };

  return (
    <div className="flex flex-col gap-3" data-testid="link-device-section">
      <p className="text-xs text-muted leading-normal">
        {t("linkDevice.description")}
      </p>

      {phase.kind === "idle" && (
        <div>
          <Button data-testid="link-device-button" size="sm" onClick={() => setPhase({ kind: "pin" })}>
            {t("linkDevice.start")}
          </Button>
        </div>
      )}

      {phase.kind === "pin" && (
        <div className="flex flex-col gap-2 max-w-xs">
          <TextInput
            data-testid="link-device-pin-input"
            label={t("linkDevice.pinLabel")}
            type="password"
            autoFocus
            value={pin}
            onChange={setPin}
            error={pinError ?? undefined}
          />
          <div className="flex gap-2">
            <Button data-testid="link-device-pin-submit" size="sm" isLoading={busy} disabled={pin.length === 0} onClick={start}>
              {t("linkDevice.showCode")}
            </Button>
            <Button data-testid="link-device-cancel" size="sm" variant="ghost" onClick={() => stop()}>
              {t("common:actions.cancel")}
            </Button>
          </div>
        </div>
      )}

      {phase.kind === "showing" && (
        <ShowingPane
          handle={phase.handle}
          status={phase.status}
          busy={busy}
          onApprove={approve}
          onReject={reject}
          onRestart={() => setPhase({ kind: "pin" })}
          onCancel={() => stop(phase.handle)}
        />
      )}

      {phase.kind === "linked" && (
        <div className="flex flex-col gap-2" data-testid="link-device-done">
          <p className="text-xs text-fg">
            {t("linkDevice.linked", { name: phase.deviceName ?? t("linkDevice.unnamedDevice") })}
          </p>
          <div>
            <Button size="sm" variant="ghost" onClick={() => stop()}>
              {t("linkDevice.done")}
            </Button>
          </div>
        </div>
      )}

      {phase.kind === "error" && (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-danger" data-testid="link-device-error">
            {phase.message}
          </p>
          <div>
            <Button size="sm" variant="ghost" onClick={() => stop()}>
              {t("common:actions.back")}
            </Button>
          </div>
        </div>
      )}
    </div>
  );
};

const ShowingPane: React.FC<{
  handle: api.DeviceLinkHandle;
  status: api.DeviceLinkStatus;
  busy: boolean;
  onApprove: (handle: api.DeviceLinkHandle, deviceName: string | null) => void;
  onReject: (handle: api.DeviceLinkHandle, requestId: string) => void;
  onRestart: () => void;
  onCancel: () => void;
}> = ({ handle, status, busy, onApprove, onReject, onRestart, onCancel }) => {
  const { t } = useTranslation("settings");
  const [secondsLeft, setSecondsLeft] = useState(() => Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
  // A display clock, not a poll: it ticks the visible countdown only.
  useEffect(() => {
    const timer = window.setInterval(() => {
      setSecondsLeft(Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [handle.expires_at]);

  if (status.state === "ready_to_approve") {
    const name = status.device_name ?? t("linkDevice.unnamedDevice");
    return (
      <div className="flex flex-col gap-3 border border-accent bg-surface p-4 rounded-lg" data-testid="link-device-approve-card">
        <p className="text-sm text-fg">{t("linkDevice.wantsToSignIn", { name })}</p>
        <p className="text-xs text-muted">{t("linkDevice.approveHint")}</p>
        <div className="flex gap-2">
          <Button data-testid="link-device-approve" size="sm" isLoading={busy} onClick={() => onApprove(handle, status.device_name)}>
            {t("linkDevice.approve")}
          </Button>
          <Button data-testid="link-device-reject" size="sm" variant="danger" disabled={busy} onClick={() => onReject(handle, status.request_id)}>
            {t("linkDevice.reject")}
          </Button>
        </div>
      </div>
    );
  }

  if (status.state === "tampered" || status.state === "expired") {
    return (
      <div className="flex flex-col gap-2" data-testid={`link-device-${status.state}`}>
        <p className={status.state === "tampered" ? "text-xs text-danger" : "text-xs text-muted"}>
          {status.state === "tampered" ? t("linkDevice.tampered") : t("linkDevice.expired")}
        </p>
        <div className="flex gap-2">
          <Button size="sm" onClick={onRestart}>
            {t("linkDevice.newCode")}
          </Button>
          <Button size="sm" variant="ghost" onClick={onCancel}>
            {t("common:actions.cancel")}
          </Button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3" data-testid="link-device-showing">
      <div className="self-start p-3 bg-white rounded-lg">
        <QRCodeSVG
          data-testid="link-device-qr"
          value={handle.qr_payload}
          size={192}
          bgColor={QR_LIGHT}
          fgColor={QR_DARK}
          marginSize={2}
          level="M"
        />
      </div>
      <div className="flex items-center gap-2">
        <LoadingSpinner size="sm" />
        <span className="text-xs font-mono text-muted" data-testid="link-device-status">
          {status.state === "claimed"
            ? t("linkDevice.claimed", { name: status.device_name ?? t("linkDevice.unnamedDevice") })
            : secondsLeft > 0
              ? t("linkDevice.scanPrompt", { seconds: secondsLeft })
              : t("linkDevice.expired")}
        </span>
      </div>
      <details className="text-xs text-muted">
        <summary className="cursor-pointer">{t("linkDevice.cantScan")}</summary>
        <p className="mt-2">{t("linkDevice.cantScanHint")}</p>
        <code className="block mt-1 break-all select-all text-fg" data-testid="link-device-payload">
          {handle.qr_payload}
        </code>
      </details>
      <div>
        <Button size="sm" variant="ghost" onClick={onCancel}>
          {t("common:actions.cancel")}
        </Button>
      </div>
    </div>
  );
};
