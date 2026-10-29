import { errorMessage } from "../../utils/errorMessage";
import React, { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { Checkbox } from "../ui/Checkbox";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import * as api from "../../services/api";

interface EnrollmentGateScreenProps {
  /// Signed in by a QR device link (#1207): request at once and wait for the
  /// QR's device to approve; there is no code to read out.
  linked?: boolean;
  userId: string;
  /// Required confirmation in the soft-recovery flow.
  userEmail: string;
  onEnrolled: () => void;
  onCancel: () => void;
  /// After a destructive soft-recovery reset; the caller shows the new Secret
  /// Key ONCE before the main app.
  onResetComplete: (newSecretKey: string) => void;
}

type GatePhase =
  | { phase: "choose" }
  | {
    phase: "awaiting-approval";
    requestId: string;
    verificationCode: string;
    expiresAt: string;
  }
  | { phase: "secret-key-fallback" }
  | { phase: "reset-confirm" }
  | { phase: "rejected" }
  | { phase: "expired" }
  | { phase: "error"; message: string };

export const EnrollmentGateScreen: React.FC<EnrollmentGateScreenProps> = ({
  linked = false,
  userId,
  userEmail,
  onEnrolled,
  onCancel,
  onResetComplete,
}) => {
  const { t } = useTranslation("auth");
  const [state, setState] = useState<GatePhase>({ phase: "choose" });
  const [isStarting, setIsStarting] = useState(false);
  // Bumped whenever a wait is abandoned (cancel, unmount, restart). The awaited
  // promise can't be cancelled, so a late answer from a superseded request is
  // ignored.
  const waitGenerationRef = useRef(0);

  useEffect(() => {
    return () => {
      waitGenerationRef.current += 1;
    };
  }, []);

  // One awaited call; Rust waits with backoff, bounded by the request's TTL.
  const awaitApproval = (requestId: string) => {
    waitGenerationRef.current += 1;
    const generation = waitGenerationRef.current;
    void (async () => {
      try {
        const status = await api.awaitEnrollmentApproval(requestId);
        if (waitGenerationRef.current !== generation) {
          return;
        }
        if (status.status === "approved") {
          onEnrolled();
        } else if (status.status === "rejected") {
          setState({ phase: "rejected" });
        } else if (status.status === "expired") {
          setState({ phase: "expired" });
        }
      } catch (err) {
        if (waitGenerationRef.current !== generation) {
          return;
        }
        console.error("[enrollment] approval wait failed:", err);
        setState({ phase: "expired" });
      }
    })();
  };

  const handleStartApproval = async () => {
    if (isStarting) {
      return;
    }
    setIsStarting(true);
    try {
      const handle = await api.startDeviceEnrollment(userId);
      setState({
        phase: "awaiting-approval",
        requestId: handle.request_id,
        verificationCode: handle.verification_code,
        expiresAt: handle.expires_at,
      });
      awaitApproval(handle.request_id);
    } catch (err) {
      // Surface the backend's reason; the "error" pane routes "sign in again"
      // cases on it.
      const message = errorMessage(err) || t("enroll.startFailed");
      setState({ phase: "error", message });
    } finally {
      setIsStarting(false);
    }
  };

  // A linked sign-in skips the chooser.
  useEffect(() => {
    if (linked) {
      void handleStartApproval();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [linked]);

  const restart = () => {
    waitGenerationRef.current += 1;
    setState({ phase: "choose" });
  };

  // A distinct tint from the OTP screen so users don't think the code was wrong.
  return (
    <div
      data-testid="enrollment-gate-screen"
      className="flex flex-col h-full w-full bg-bg"
      style={{
        position: "relative",
      }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.45, pointerEvents: "none" }}>
        <DotMatrix speed={1.4} />
      </div>
      <TitleBar />

      <div
        className="flex-1 flex justify-center overflow-y-auto"
        style={{ position: "relative", zIndex: 1, padding: "1rem" }}
      >
        <Card
          padding="lg"
          style={{
            width: "100%",
            maxWidth: 460,
            // Auto margins (not items-center) so a card taller than the
            // viewport scrolls instead of clipping its top.
            marginTop: "auto",
            marginBottom: "auto",
            border: "2px solid var(--c-accent)",
          }}
        >
          <div className="flex flex-col gap-5">
            <div className="border-b border-line">
              <p
                className="text-sm font-mono uppercase tracking-wider mb-8 text-accent"
                style={{ letterSpacing: "0.15em" }}
              >
                {t("enroll.badge")}
              </p>
              <h1 className="text-base font-mono font-bold mt-1 mb-8 text-fg">
                {t("enroll.title")}
              </h1>
            </div>

            {state.phase === "choose" && (
              <ChoosePane
                onStartApproval={handleStartApproval}
                onUseSecretKey={() => setState({ phase: "secret-key-fallback" })}
                onCancel={onCancel}
                isStarting={isStarting}
              />
            )}

            {state.phase === "awaiting-approval" && linked && (
              <LinkedAwaitingPane onCancel={onCancel} />
            )}

            {state.phase === "awaiting-approval" && !linked && (
              <AwaitingApprovalPane
                code={state.verificationCode}
                expiresAt={state.expiresAt}
                onCancel={restart}
              />
            )}

            {state.phase === "secret-key-fallback" && (
              <SecretKeyFallbackPane
                userId={userId}
                onRecovered={onEnrolled}
                onBack={restart}
                onWantReset={() => setState({ phase: "reset-confirm" })}
              />
            )}

            {state.phase === "reset-confirm" && (
              <ResetConfirmPane
                userId={userId}
                expectedEmail={userEmail}
                onResetComplete={onResetComplete}
                onBack={restart}
              />
            )}

            {state.phase === "rejected" && (
              <ResultPane
                heading={t("enroll.rejectedHeading")}
                body={t("enroll.rejectedBody")}
                actionLabel={t("enroll.tryAgain")}
                onAction={restart}
                onCancel={onCancel}
                tone="error"
              />
            )}

            {state.phase === "expired" && (
              <ResultPane
                heading={t("enroll.expiredHeading")}
                body={t("enroll.expiredBody")}
                actionLabel={t("enroll.tryAgain")}
                onAction={restart}
                onCancel={onCancel}
                tone="muted"
              />
            )}

            {state.phase === "error" && (() => {
              // A missing/expired OTP session (e.g. after relaunch) can't be
              // retried in place: only a fresh sign-in mints the enrollment
              // session `start_device_enrollment` needs.
              const needsResignin = /session|sign in/i.test(state.message);
              return (
                <ResultPane
                  heading={t("enroll.errorHeading")}
                  body={state.message}
                  actionLabel={needsResignin ? t("enroll.signInAgain") : t("enroll.tryAgain")}
                  onAction={needsResignin ? onCancel : restart}
                  onCancel={onCancel}
                  tone="error"
                />
              );
            })()}
          </div>
        </Card>
      </div>
    </div>
  );
};

const ChoosePane: React.FC<{
  onStartApproval: () => void;
  onUseSecretKey: () => void;
  onCancel: () => void;
  isStarting: boolean;
}> = ({ onStartApproval, onUseSecretKey, onCancel, isStarting }) => {
  const { t } = useTranslation("auth");
  return (
    <div className="flex flex-col gap-3 mb-4">
      <Button
        data-testid="enroll-via-approval-button"
        onClick={onStartApproval}
        isLoading={isStarting}
        loadingText={t("enroll.requesting")}
        className="w-full mb-2"
      >
        {t("enroll.approveFromDevice")}
      </Button>
      <p className="text-xs font-mono mb-4 text-muted">
        {t("enroll.approveHint")}
      </p>

      <div
        className="border-t border-line"
        style={{
          paddingTop: "1rem",
        }}
      >
        <Button
          data-testid="enroll-via-secret-key-button"
          onClick={onUseSecretKey}
          variant="secondary"
          className="w-full mt-4"
        >
          {t("enroll.useSecretKey")}
        </Button>
        <p className="text-xs font-mono mt-4 text-muted">
          {t("enroll.useSecretKeyHint")}
        </p>
      </div>

      <Button
        data-testid="enrollment-cancel-button"
        onClick={onCancel}
        variant="primary"
        size="sm"
        className="w-full mt-12"
      >
        {t("enroll.cancelAndSwitch")}
      </Button>
    </div>
  );
};

/// Waiting on a QR-linked approval (#1207).
const LinkedAwaitingPane: React.FC<{ onCancel: () => void }> = ({ onCancel }) => {
  const { t } = useTranslation("auth");
  return (
    <div className="flex flex-col gap-4" data-testid="linked-awaiting-approval">
      <p className="text-sm font-mono text-fg">{t("link.awaitingTitle")}</p>
      <p className="text-xs font-mono text-muted">{t("link.awaitingIntro")}</p>
      <div className="flex items-center gap-2 justify-center">
        <LoadingSpinner size="sm" />
      </div>
      <Button data-testid="cancel-linked-approval-button" onClick={onCancel} variant="ghost" className="w-full">
        {t("common:actions.cancel")}
      </Button>
    </div>
  );
};

const AwaitingApprovalPane: React.FC<{
  code: string;
  expiresAt: string;
  onCancel: () => void;
}> = ({ code, expiresAt, onCancel }) => {
  const { t } = useTranslation("auth");
  const [secondsLeft, setSecondsLeft] = useState(() => secondsUntil(expiresAt));
  // A display clock ticking a rendered countdown, not a network poll.
  useEffect(() => {
    const t = window.setInterval(() => {
      setSecondsLeft(secondsUntil(expiresAt));
    }, 1000);
    return () => window.clearInterval(t);
  }, [expiresAt]);

  return (
    <div className="flex flex-col gap-4">
      <p className="text-xs font-mono text-fg">
        {t("enroll.awaitingIntro")}
      </p>
      <div
        data-testid="verification-code-display"
        className="font-mono text-3xl font-bold text-center select-all bg-surface border-2 border-accent text-accent"
        style={{
          borderRadius: "0.5rem",
          padding: "1.5rem",
          letterSpacing: "0.4em",
        }}
      >
        {code}
      </div>
      <div className="flex items-center gap-2 justify-center">
        <LoadingSpinner size="sm" />
        <span className="text-xs font-mono text-muted">
          {secondsLeft > 0
            ? t("enroll.awaitingCountdown", { time: formatCountdown(secondsLeft) })
            : t("enroll.awaitingExpired")}
        </span>
      </div>
      <Button
        data-testid="cancel-awaiting-approval-button"
        onClick={onCancel}
        variant="ghost"
        className="w-full"
      >
        {t("common:actions.cancel")}
      </Button>
    </div>
  );
};

const SecretKeyFallbackPane: React.FC<{
  userId: string;
  onRecovered: () => void;
  onBack: () => void;
  onWantReset: () => void;
}> = ({ userId, onRecovered, onBack, onWantReset }) => {
  const { t } = useTranslation("auth");
  const [value, setValue] = useState("");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleRecover = async () => {
    if (!value.trim() || isLoading) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.recoverWithSecretKey(userId, value.trim());
      onRecovered();
    } catch (err) {
      const message = errorMessage(err) || t("recover.failed");
      setError(message);
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <p
        className="text-xs font-mono text-fg"
        style={{ lineHeight: 1.6 }}
      >
        {t("recover.intro")}
      </p>
      <p
        className="text-xs font-mono mb-2 text-muted"
        style={{ lineHeight: 1.6 }}
      >
        {t("recover.hint")}
      </p>
      <TextInput
        data-testid="secret-key-recovery-input"
        label={t("recover.inputLabel")}
        value={value.trim()}
        onChange={(v) => {
          setValue(v);
          setError(null);
        }}
        placeholder="A3-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX"
        error={error ?? undefined}
        disabled={isLoading}
        className="mb-4"
      />
      <Button
        data-testid="recover-with-secret-key-button"
        onClick={handleRecover}
        disabled={!value.trim()}
        isLoading={isLoading}
        loadingText={t("recover.recovering")}
        className="w-full mb-4"
      >
        {t("recover.submit")}
      </Button>
      <Button
        data-testid="secret-key-fallback-back-button"
        onClick={onBack}
        variant="ghost"
        disabled={isLoading}
        className="w-full"
      >
        {t("common:actions.back")}
      </Button>

      <div
        style={{
          marginTop: "0.75rem",
        }}
      >
        <Button
          data-testid="want-reset-identity-button"
          variant="danger"
          onClick={onWantReset}
          disabled={isLoading}
          className="text-xs font-mono"
        >
          {t("recover.wantReset")}
        </Button>
      </div>
    </div>
  );
};

const ResetConfirmPane: React.FC<{
  userId: string;
  expectedEmail: string;
  onResetComplete: (newSecretKey: string) => void;
  onBack: () => void;
}> = ({ userId, expectedEmail, onResetComplete, onBack }) => {
  const { t } = useTranslation("auth");
  const [typedEmail, setTypedEmail] = useState("");
  const [acknowledged, setAcknowledged] = useState(false);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canSubmit =
    acknowledged && typedEmail.trim().toLowerCase() === expectedEmail.trim().toLowerCase();

  const handleReset = async () => {
    if (!canSubmit || isLoading) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      const newKey = await api.resetIdentityAndRecover(userId, typedEmail.trim());
      onResetComplete(newKey);
    } catch (err) {
      const message = errorMessage(err) || t("reset.failed");
      setError(message);
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2 className="text-sm font-mono font-bold text-danger">
          {t("reset.title")}
        </h2>
        <p
          className="text-xs mt-2 font-mono text-fg"
          style={{ lineHeight: 1.6 }}
        >
          {t("reset.body")}
        </p>
        <p
          className="text-xs mt-2 font-mono mb-4 text-muted"
          style={{ lineHeight: 1.6 }}
        >
          {t("reset.keepNote")}
        </p>
      </div>

      <Checkbox
        data-testid="reset-acknowledge-checkbox"
        label={t("reset.acknowledge")}
        checked={acknowledged}
        onChange={setAcknowledged}
        disabled={isLoading}
        className="mb-2"
      />

      <TextInput
        data-testid="reset-confirm-email-input"
        label={t("reset.emailLabel")}
        value={typedEmail}
        onChange={(v) => {
          setTypedEmail(v);
          setError(null);
        }}
        placeholder={expectedEmail}
        error={error ?? undefined}
        disabled={isLoading}
      />

      <Button
        data-testid="confirm-reset-identity-button"
        onClick={handleReset}
        disabled={!canSubmit}
        isLoading={isLoading}
        loadingText={t("reset.resetting")}
        variant="danger"
        className="w-full mt-2"
      >
        {t("reset.submit")}
      </Button>
      <Button
        data-testid="reset-back-button"
        onClick={onBack}
        variant="ghost"
        disabled={isLoading}
        className="w-full"
      >
        {t("common:actions.back")}
      </Button>
    </div>
  );
};

const ResultPane: React.FC<{
  heading: string;
  body: string;
  actionLabel: string;
  onAction: () => void;
  onCancel: () => void;
  tone: "error" | "muted";
}> = ({ heading, body, actionLabel, onAction, onCancel, tone }) => {
  const { t } = useTranslation("auth");
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className={`text-sm font-mono font-bold ${tone === "error" ? "text-danger" : "text-fg"}`}
        >
          {heading}
        </h2>
        <p
          className="text-xs mt-2 font-mono text-muted"
          style={{ lineHeight: 1.6 }}
        >
          {body}
        </p>
      </div>
      <Button
        data-testid="enrollment-result-action-button"
        onClick={onAction}
        className="w-full"
      >
        {actionLabel}
      </Button>
      <Button
        data-testid="enrollment-result-cancel-button"
        onClick={onCancel}
        variant="ghost"
        className="w-full"
      >
        {t("enroll.signInAsSomeoneElse")}
      </Button>
    </div>
  );
};

function secondsUntil(rfc3339: string): number {
  const target = new Date(rfc3339).getTime();
  const now = Date.now();
  return Math.max(0, Math.floor((target - now) / 1000));
}

// Clock-style M:SS; the "… left" wording lives in the catalogue.
function formatCountdown(s: number): string {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")}`;
}
