import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { QRCodeSVG } from "qrcode.react";
import { observer } from "mobx-react-lite";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { InputOtp } from "../components/ui/InputOtp";
import { LoadingSpinner } from "../components/ui/LoaderSpinner";
import { writeClipboardText } from "../bridge";
import { appStore } from "../stores/appStore";
import * as api from "../services/api";
import { errorMessage } from "../utils/errorMessage";

/**
 * Link a new device (#1207), one step per view — the desktop twin of mobile's
 * self/link-device: PIN → the code (QR or copyable text) → waiting → Approve
 * → done. The PIN is verified in Rust; the key moves only on Approve of a
 * request whose link tag Rust verified. Each wait is one awaited Rust call.
 */

type Step =
  | { kind: "pin" }
  | { kind: "code"; handle: api.DeviceLinkHandle; status: api.DeviceLinkStatus }
  | { kind: "linked"; name: string }
  | { kind: "error"; message: string };

const Heading: React.FC<{ title: string; subtitle?: string; testId?: string }> = ({ title, subtitle, testId }) => (
  <div className="flex flex-col gap-3" data-testid={testId}>
    <h1 className="text-xl text-fg">{title}</h1>
    {subtitle && <p className="text-sm text-muted leading-relaxed">{subtitle}</p>}
  </div>
);

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

  const approve = async (handle: api.DeviceLinkHandle, name: string) => {
    if (!userId) {
      return;
    }
    setBusy(true);
    try {
      await api.approveDeviceLink(userId, handle.link_id);
      generation.current += 1;
      setStep({ kind: "linked", name });
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
        <Heading title={t("linkDevice.pinTitle")} subtitle={t("linkDevice.pinSubtitle")} />
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
          {pinError && <p className="text-sm text-danger" data-testid="link-device-pin-error">{pinError}</p>}
        </div>
        <div>
          <Button variant="ghost" onClick={() => navigate({ to: "/security" })}>
            {t("common:actions.cancel")}
          </Button>
        </div>
      </>
    );
  } else if (step.kind === "linked") {
    body = (
      <>
        <Heading testId="link-device-done" title={t("linkDevice.linked", { name: step.name })} />
        <div>
          <Button onClick={() => navigate({ to: "/security" })}>{t("linkDevice.done")}</Button>
        </div>
      </>
    );
  } else if (step.kind === "error") {
    body = (
      <>
        <Heading testId="link-device-error" title={t("linkDevice.failed")} subtitle={step.message} />
        <div className="flex gap-2">
          <Button onClick={restart}>{t("linkDevice.newCode")}</Button>
          <Button variant="ghost" onClick={leave}>{t("common:actions.cancel")}</Button>
        </div>
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
          <Heading
            testId={`link-device-${status.state}`}
            title={tampered ? t("linkDevice.tamperedTitle") : t("linkDevice.expiredTitle")}
            subtitle={tampered ? t("linkDevice.tampered") : t("linkDevice.expired")}
          />
          <div className="flex gap-2">
            <Button onClick={restart}>{t("linkDevice.newCode")}</Button>
            <Button variant="ghost" onClick={leave}>{t("common:actions.cancel")}</Button>
          </div>
        </>
      );
    } else if (status.state === "ready_to_approve") {
      body = (
        <>
          <Heading
            testId="link-device-approve-card"
            title={t("linkDevice.wantsToSignIn", { name })}
            subtitle={t("linkDevice.approveHint")}
          />
          <div className="flex gap-2">
            <Button data-testid="link-device-approve" isLoading={busy} onClick={() => approve(handle, name)}>
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
          </div>
        </>
      );
    } else if (status.state === "claimed") {
      body = (
        <>
          <Heading testId="link-device-claimed" title={t("linkDevice.claimedTitle")} subtitle={t("linkDevice.claimed", { name })} />
          <LoadingSpinner size="sm" />
          <div>
            <Button variant="ghost" onClick={leave}>{t("common:actions.cancel")}</Button>
          </div>
        </>
      );
    } else {
      body = <ShowCode handle={handle} onCancel={leave} />;
    }
  }

  return (
    <PageShell title={t("linkDevice.heading")} scrollable>
      <div className="flex justify-center px-6 py-12">
        <div className="flex flex-col gap-10 w-full max-w-md" data-testid="link-device-page">
          {body}
        </div>
      </div>
    </PageShell>
  );
});

/** The code, as a QR (default) or as text with one-click copy. */
const ShowCode: React.FC<{ handle: api.DeviceLinkHandle; onCancel: () => void }> = ({ handle, onCancel }) => {
  const { t } = useTranslation("settings");
  const [mode, setMode] = useState<"qr" | "code">("qr");
  const [copied, setCopied] = useState(false);
  const [secondsLeft, setSecondsLeft] = useState(() => Math.max(0, handle.expires_at - Math.floor(Date.now() / 1000)));
  // A display clock for the countdown; it touches no network.
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
      <Heading title={t("linkDevice.showTitle")} subtitle={mode === "qr" ? t("linkDevice.showSubtitle") : t("linkDevice.codeSubtitle")} />
      <div className="flex gap-2" role="tablist">
        <Button data-testid="link-device-tab-qr" size="sm" variant={mode === "qr" ? "primary" : "ghost"} onClick={() => setMode("qr")}>
          {t("linkDevice.tabQr")}
        </Button>
        <Button data-testid="link-device-tab-code" size="sm" variant={mode === "code" ? "primary" : "ghost"} onClick={() => setMode("code")}>
          {t("linkDevice.tabCode")}
        </Button>
      </div>
      {mode === "qr" ? (
        <div className="self-start p-3 rounded-lg bg-accent" data-testid="link-device-showing">
          {/* Dark modules on the accent: themed, and still normal polarity
              (dark on light), which every camera decoder reads. */}
          <QRCodeSVG data-testid="link-device-qr" value={handle.qr_payload} size={208} bgColor="var(--c-accent)" fgColor="var(--c-bg)" marginSize={2} level="M" />
        </div>
      ) : (
        <button
          type="button"
          data-testid="link-device-copy"
          onClick={copy}
          className="text-start flex flex-col gap-3 p-5 rounded-lg border border-accent bg-surface hover:bg-hover"
        >
          <code className="font-mono text-base text-fg break-all leading-relaxed" data-testid="link-device-payload">
            {handle.qr_payload}
          </code>
          <span className={copied ? "text-xs font-mono text-accent" : "text-xs font-mono text-muted"}>
            {copied ? t("linkDevice.copied") : t("linkDevice.clickToCopy")}
          </span>
        </button>
      )}
      <p className="text-sm text-muted" data-testid="link-device-status">
        {secondsLeft > 0 ? t("linkDevice.expiresIn", { time: `0:${String(secondsLeft).padStart(2, "0")}` }) : t("linkDevice.expiredTitle")}
      </p>
      <div>
        <Button variant="ghost" onClick={onCancel}>{t("common:actions.cancel")}</Button>
      </div>
    </>
  );
};
