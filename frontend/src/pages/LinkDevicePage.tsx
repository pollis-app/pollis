import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { FormActions } from "../components/ui/FormActions";
import { InputOtp } from "../components/ui/InputOtp";
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { ThemedQr } from "../components/ui/ThemedQr";
import { PressButton } from "../components/ui/PressButton";
import { FOCUS_RING } from "../components/ui/focusRing";
import { LinkedHistoryRequest } from "../components/HistoryTransfer/LinkedHistoryRequest";
import { writeClipboardText } from "../bridge";
import { appStore } from "../stores/appStore";
import * as api from "../services/api";
import { errorMessage } from "../utils/errorMessage";

/** Link a new device (#1207): PIN → code → wait → Approve; Rust verifies the PIN and link tag. */

type Step =
  | { kind: "pin" }
  | { kind: "code"; handle: api.DeviceLinkHandle; status: api.DeviceLinkStatus }
  | { kind: "linked"; name: string; deviceId: string | null }
  | { kind: "error"; message: string };

export const LinkDevicePage: React.FC = observer(() => {
  const { t } = useTranslation("settings");
  const navigate = useNavigate();
  const userId = appStore.currentUser?.id ?? null;
  const [step, setStep] = useState<Step>({ kind: "pin" });
  const [pin, setPin] = useState("");
  const [pinError, setPinError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const generation = useRef(0);

  useEffect(() => {
    return () => {
      generation.current += 1;
    };
  }, []);

  const follow = (handle: api.DeviceLinkHandle, since: api.DeviceLinkStatus["state"]) => {
    if (!userId) {
      return;
    }
    generation.current += 1;
    const mine = generation.current;
    void (async () => {
      try {
        const next = await api.awaitDeviceLink(userId, handle.link_id, since);
        if (generation.current !== mine) {
          return;
        }
        setStep({ kind: "code", handle, status: next });
        if (next.state === "open" || next.state === "claimed") {
          follow(handle, next.state);
        }
      } catch (err) {
        if (generation.current === mine) {
          setStep({ kind: "error", message: errorMessage(err) || t("linkDevice.failed") });
        }
      }
    })();
  };

  const submitPin = async (value: string) => {
    if (!userId || busy) {
      return;
    }
    setBusy(true);
    setPinError(null);
    try {
      const handle = await api.createDeviceLink(userId, value);
      setStep({ kind: "code", handle, status: { state: "open" } });
      follow(handle, "open");
    } catch (err) {
      setPinError(errorMessage(err) || t("linkDevice.pinFailed"));
    } finally {
      setPin("");
      setBusy(false);
    }
  };

  const leave = async () => {
    generation.current += 1;
    if (step.kind === "code") {
      await api.cancelDeviceLink(step.handle.link_id).catch(() => undefined);
    }
    navigate({ to: "/security" });
  };

  const restart = () => {
    generation.current += 1;
    setPin("");
    setPinError(null);
    setStep({ kind: "pin" });
  };

  const approve = async (handle: api.DeviceLinkHandle, name: string, deviceId: string | null) => {
    if (!userId) {
      return;
    }
    setBusy(true);
    try {
      await api.approveDeviceLink(userId, handle.link_id);
      generation.current += 1;
      setStep({ kind: "linked", name, deviceId });
    } catch (err) {
      setStep({ kind: "error", message: errorMessage(err) || t("linkDevice.failed") });
    } finally {
      setBusy(false);
    }
  };

  let body: React.ReactNode;
  if (step.kind === "pin") {
    body = (
      <>
        <FormLead title={t("linkDevice.pinTitle")}>{t("linkDevice.pinSubtitle")}</FormLead>
        <div className="flex flex-col gap-3" data-testid="link-device-pin-input">
          <InputOtp
            length={4}
            value={pin}
            onChange={(v) => {
              setPin(v);
              if (v.length === 4) {
                void submitPin(v);
              }
            }}
            disabled={busy}
            mask
            autoFocus
          />
          {pinError && <p className="text-body text-danger" data-testid="link-device-pin-error">{pinError}</p>}
        </div>
        <FormActions cancel={{ onClick: () => navigate({ to: "/security" }) }} />
      </>
    );
  } else if (step.kind === "linked") {
    body = (
      <>
        <FormLead testId="link-device-done" title={t("linkDevice.linked", { name: step.name })} />
        {/* #1735: the new device may ask for this one's message history right away. */}
        <LinkedHistoryRequest deviceId={step.deviceId} deviceName={step.name} />
        <FormActions>
          <Button onClick={() => navigate({ to: "/security" })}>{t("linkDevice.done")}</Button>
        </FormActions>
      </>
    );
  } else if (step.kind === "error") {
    body = (
      <>
        <FormLead testId="link-device-error" title={t("linkDevice.failed")}>{step.message}</FormLead>
        <FormActions cancel={{ onClick: leave }}>
          <Button onClick={restart}>{t("linkDevice.newCode")}</Button>
        </FormActions>
      </>
    );
  } else {
    const { handle, status } = step;
    const name =
      (status.state === "claimed" || status.state === "ready_to_approve" ? status.device_name : null) ??
      t("linkDevice.unnamedDevice");
    if (status.state === "tampered" || status.state === "expired") {
      const tampered = status.state === "tampered";
      body = (
        <>
          <FormLead
            testId={`link-device-${status.state}`}
            title={tampered ? t("linkDevice.tamperedTitle") : t("linkDevice.expiredTitle")}
          >
            {tampered ? t("linkDevice.tampered") : t("linkDevice.expired")}
          </FormLead>
          <FormActions cancel={{ onClick: leave }}>
            <Button onClick={restart}>{t("linkDevice.newCode")}</Button>
          </FormActions>
        </>
      );
    } else if (status.state === "ready_to_approve") {
      body = (
        <>
          <FormLead testId="link-device-approve-card" title={t("linkDevice.wantsToSignIn", { name })}>{t("linkDevice.approveHint")}</FormLead>
          <FormActions>
            <Button data-testid="link-device-approve" isLoading={busy} onClick={() => approve(handle, name, status.new_device_id)}>
              {t("linkDevice.approve")}
            </Button>
            <Button
              data-testid="link-device-reject"
              variant="danger"
              disabled={busy}
              onClick={async () => {
                await api.rejectDeviceEnrollment(status.request_id).catch(() => undefined);
                await leave();
              }}
            >
              {t("linkDevice.reject")}
            </Button>
          </FormActions>
        </>
      );
    } else if (status.state === "claimed") {
      body = (
        <>
          <FormLead testId="link-device-claimed" title={t("linkDevice.claimedTitle")}>{t("linkDevice.claimed", { name })}</FormLead>
          <LoadingSpinner size="sm" />
          <FormActions cancel={{ onClick: leave }} />
        </>
      );
    } else {
      body = <ShowCode handle={handle} onCancel={leave} />;
    }
  }

  return (
    <PageShell title={t("linkDevice.heading")} scrollable>
      <PageColumn spacing="sections" data-testid="link-device-page">
        {body}
      </PageColumn>
    </PageShell>
  );
});

/** The code, as a QR (default) or as text with one-click copy. */
const ShowCode: React.FC<{ handle: api.DeviceLinkHandle; onCancel: () => void }> = ({ handle, onCancel }) => {
  const { t } = useTranslation("settings");
  const [mode, setMode] = useState<"qr" | "code">("qr");
  const [copied, setCopied] = useState(false);
  const [secondsLeft, setSecondsLeft] = useState(() => Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
  useEffect(() => {
    const timer = window.setInterval(() => {
      setSecondsLeft(Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
    }, 1000);
    return () => window.clearInterval(timer);
  }, [handle.expires_at]);

  const copy = async () => {
    if (await writeClipboardText(handle.qr_payload)) {
      setCopied(true);
    }
  };

  return (
    <>
      <FormLead title={t("linkDevice.showTitle")}>
        {mode === "qr" ? t("linkDevice.showSubtitle") : t("linkDevice.codeSubtitle")}
      </FormLead>
      <div className="flex gap-2" role="tablist">
        <Button data-testid="link-device-tab-qr" size="sm" variant={mode === "qr" ? "primary" : "ghost"} onClick={() => setMode("qr")}>
          {t("linkDevice.tabQr")}
        </Button>
        <Button data-testid="link-device-tab-code" size="sm" variant={mode === "code" ? "primary" : "ghost"} onClick={() => setMode("code")}>
          {t("linkDevice.tabCode")}
        </Button>
      </div>
      {mode === "qr" ? (
        <div className="self-start" data-testid="link-device-showing">
          <ThemedQr data-testid="link-device-qr" value={handle.qr_payload} size={208} />
        </div>
      ) : (
        <PressButton
          data-testid="link-device-copy"
          activateOn="click"
          onClick={copy}
          className={`text-start flex flex-col gap-3 p-5 rounded-panel border border-accent bg-surface hover:bg-hover ${FOCUS_RING}`}
        >
          <code className="font-mono text-row text-fg break-all leading-relaxed" data-testid="link-device-payload">
            {handle.qr_payload}
          </code>
          <span className={copied ? "text-secondary font-mono text-accent" : "text-secondary font-mono text-muted"}>
            {copied ? t("linkDevice.copied") : t("linkDevice.clickToCopy")}
          </span>
        </PressButton>
      )}
      <p className="text-body text-muted" data-testid="link-device-status">
        {secondsLeft > 0 ? t("linkDevice.expiresIn", { time: `${Math.floor(secondsLeft / 60)}:${String(secondsLeft % 60).padStart(2, "0")}` }) : t("linkDevice.expiredTitle")}
      </p>
      <FormActions cancel={{ onClick: onCancel }} />
    </>
  );
};
