import React, { useEffect, useRef, useState } from "react";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { Checkbox } from "../ui/Checkbox";
import { LoadingSpinner } from "../ui/LoaderSpinner";
import * as api from "../../services/api";

interface EnrollmentGateScreenProps {
  userId: string;
  /// Email address the user just signed in with, used as the required
  /// confirmation in the soft-recovery flow.
  userEmail: string;
  /// Called once enrollment completes successfully (status === 'approved').
  onEnrolled: () => void;
  /// Called when the user gives up (e.g. cancel button) — returns to login.
  onCancel: () => void;
  /// Called after a destructive soft-recovery reset. The caller is
  /// expected to display the freshly-generated Secret Key ONCE before
  /// transitioning to the main app.
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

const POLL_INTERVAL_MS = 2000;

export const EnrollmentGateScreen: React.FC<EnrollmentGateScreenProps> = ({
  userId,
  userEmail,
  onEnrolled,
  onCancel,
  onResetComplete,
}) => {
  const [state, setState] = useState<GatePhase>({ phase: "choose" });
  const [isStarting, setIsStarting] = useState(false);
  const pollTimerRef = useRef<number | null>(null);

  // Stop polling on unmount or whenever the phase changes away from awaiting.
  useEffect(() => {
    return () => {
      if (pollTimerRef.current !== null) {
        window.clearInterval(pollTimerRef.current);
        pollTimerRef.current = null;
      }
    };
  }, []);

  const startPolling = (requestId: string) => {
    if (pollTimerRef.current !== null) {
      window.clearInterval(pollTimerRef.current);
    }
    pollTimerRef.current = window.setInterval(async () => {
      try {
        const status = await api.pollEnrollmentStatus(requestId);
        if (status.status === "approved") {
          if (pollTimerRef.current !== null) {
            window.clearInterval(pollTimerRef.current);
            pollTimerRef.current = null;
          }
          onEnrolled();
        } else if (status.status === "rejected") {
          if (pollTimerRef.current !== null) {
            window.clearInterval(pollTimerRef.current);
            pollTimerRef.current = null;
          }
          setState({ phase: "rejected" });
        } else if (status.status === "expired") {
          if (pollTimerRef.current !== null) {
            window.clearInterval(pollTimerRef.current);
            pollTimerRef.current = null;
          }
          setState({ phase: "expired" });
        }
      } catch (err) {
        console.error("[enrollment] poll failed:", err);
      }
    }, POLL_INTERVAL_MS) as unknown as number;
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
      startPolling(handle.request_id);
    } catch (err) {
      const message = err instanceof Error ? err.message : "Failed to start enrollment";
      setState({ phase: "error", message });
    } finally {
      setIsStarting(false);
    }
  };

  const restart = () => {
    if (pollTimerRef.current !== null) {
      window.clearInterval(pollTimerRef.current);
      pollTimerRef.current = null;
    }
    setState({ phase: "choose" });
  };

  return (
    <div
      data-testid="enrollment-gate-screen"
      className="flex flex-col h-full w-full"
      style={{
        // Distinct background tint vs the OTP screen so users don't think
        // they entered the wrong code.
        background: "var(--c-bg)",
        position: "relative",
      }}
    >
      {/* Faster, more energetic dot matrix to differentiate from OTP screen */}
      <div style={{ position: "absolute", inset: 0, opacity: 0.45, pointerEvents: "none" }}>
        <DotMatrix speed={1.4} />
      </div>
      <TitleBar />

      <div
        className="flex-1 flex items-center justify-center"
        style={{ position: "relative", zIndex: 1, padding: "1rem" }}
      >
        <Card
          padding="lg"
          style={{
            width: "100%",
            maxWidth: 460,
            // Visually distinct accent border so this doesn't blend with
            // the OTP card.
            border: "2px solid var(--c-accent)",
          }}
        >
          <div className="flex flex-col gap-5">
            <div style={{
              borderBottom: "1px solid var(--c-border)",
            }}>
              <p
                className="text-sm font-mono uppercase tracking-wider mb-8"
                style={{ color: "var(--c-accent)", letterSpacing: "0.15em" }}
              >
                New device
              </p>
              <h1
                className="text-base font-mono font-bold mt-1 mb-8"
                style={{ color: "var(--c-text)" }}
              >
                Authorize this device to add it to your account
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

            {state.phase === "awaiting-approval" && (
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
                heading="Request rejected"
                body="One of your other devices rejected this enrollment. If that wasn't you, change your email password immediately."
                actionLabel="Try again"
                onAction={restart}
                onCancel={onCancel}
                tone="error"
              />
            )}

            {state.phase === "expired" && (
              <ResultPane
                heading="Request expired"
                body="The 10-minute approval window passed. You can start a new request."
                actionLabel="Try again"
                onAction={restart}
                onCancel={onCancel}
                tone="muted"
              />
            )}

            {state.phase === "error" && (
              <ResultPane
                heading="Something went wrong"
                body={state.message}
                actionLabel="Try again"
                onAction={restart}
                onCancel={onCancel}
                tone="error"
              />
            )}
          </div>
        </Card>
      </div>
    </div>
  );
};

// ── Sub-panes ──────────────────────────────────────────────────────────────

const ChoosePane: React.FC<{
  onStartApproval: () => void;
  onUseSecretKey: () => void;
  onCancel: () => void;
  isStarting: boolean;
}> = ({ onStartApproval, onUseSecretKey, onCancel, isStarting }) => (
  <div className="flex flex-col gap-3 mb-4">
    <Button
      data-testid="enroll-via-approval-button"
      onClick={onStartApproval}
      isLoading={isStarting}
      loadingText="Requesting…"
      className="w-full mb-2"
    >
      Approve from another device
    </Button>
    <p
      className="text-xs font-mono mb-4"
      style={{ color: "var(--c-text-muted)" }}
    >
      You'll see a 6-digit code here. Open Pollis on a device you're already
      signed in to and confirm the code.
    </p>

    <div
      style={{
        borderTop: "1px solid var(--c-border)",
        paddingTop: "1rem",
      }}
    >
      <Button
        data-testid="enroll-via-secret-key-button"
        onClick={onUseSecretKey}
        variant="secondary"
        className="w-full mt-4"
      >
        Use my Secret Key instead
      </Button>
      <p
        className="text-xs font-mono mt-4"
        style={{ color: "var(--c-text-muted)" }}
      >
        For when you don't have any other Pollis device with you.
      </p>
    </div>

    <Button
      data-testid="enrollment-cancel-button"
      onClick={onCancel}
      variant="primary"
      size="sm"
      className="w-full mt-12"
    >
      Cancel and sign in as someone else
    </Button>
  </div>
);

const AwaitingApprovalPane: React.FC<{
  code: string;
  expiresAt: string;
  onCancel: () => void;
}> = ({ code, expiresAt, onCancel }) => {
  const [secondsLeft, setSecondsLeft] = useState(() => secondsUntil(expiresAt));
  useEffect(() => {
    const t = window.setInterval(() => {
      setSecondsLeft(secondsUntil(expiresAt));
    }, 1000);
    return () => window.clearInterval(t);
  }, [expiresAt]);

  return (
    <div className="flex flex-col gap-4">
      <p className="text-xs font-mono" style={{ color: "var(--c-text)" }}>
        Open Pollis on another device that's already signed in. You'll see a
        prompt asking you to confirm this code:
      </p>
      <div
        data-testid="verification-code-display"
        className="font-mono text-3xl font-bold text-center select-all"
        style={{
          background: "var(--c-surface)",
          border: "2px solid var(--c-accent)",
          borderRadius: "0.5rem",
          padding: "1.5rem",
          color: "var(--c-accent)",
          letterSpacing: "0.4em",
        }}
      >
        {code}
      </div>
      <div className="flex items-center gap-2 justify-center">
        <LoadingSpinner size="sm" />
        <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          Waiting for approval…{" "}
          {secondsLeft > 0 ? `(${formatSecondsLeft(secondsLeft)})` : "(expired)"}
        </span>
      </div>
      <Button
        data-testid="cancel-awaiting-approval-button"
        onClick={onCancel}
        variant="ghost"
        className="w-full"
      >
        Cancel
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
      const message = err instanceof Error ? err.message : "Recovery failed";
      setError(message);
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <p
        className="text-xs font-mono"
        style={{ color: "var(--c-text)", lineHeight: 1.6 }}
      >
        Paste the Secret Key from your Emergency Kit.
      </p>
      <p
        className="text-xs font-mono mb-2"
        style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
      >
        Recovery can take a few seconds while this device is registered.
      </p>
      <TextInput
        data-testid="secret-key-recovery-input"
        label="Secret Key"
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
        loadingText="Recovering…"
        className="w-full mb-4"
      >
        Recover account
      </Button>
      <Button
        data-testid="secret-key-fallback-back-button"
        onClick={onBack}
        variant="ghost"
        disabled={isLoading}
        className="w-full"
      >
        Back
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
          I've lost my Secret Key — reset my account
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
      const message = err instanceof Error ? err.message : "Reset failed";
      setError(message);
    } finally {
      setIsLoading(false);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div>
        <h2
          className="text-sm font-mono font-bold"
          style={{ color: "#ff6b6b" }}
        >
          Reset this account
        </h2>
        <p
          className="text-xs mt-2 font-mono"
          style={{ color: "var(--c-text)", lineHeight: 1.6 }}
        >
          Your messages on this device will be wiped for good. You'll
          leave your groups (admins can invite you back) and your other
          devices will be signed out.
        </p>
        <p
          className="text-xs mt-2 font-mono mb-4"
          style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
        >
          You'll keep your email and username, and a new Secret Key will
          be shown once. Save it somewhere safe — lose it and the only
          way back in is to do all of this again.
        </p>
      </div>

      <Checkbox
        data-testid="reset-acknowledge-checkbox"
        label="I understand that all of my messages will be removed from this device and I will be removed from all of my groups and conversations."
        checked={acknowledged}
        onChange={setAcknowledged}
        disabled={isLoading}
        className="mb-2"
      />

      <TextInput
        data-testid="reset-confirm-email-input"
        label={`Type your email to confirm`}
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
        loadingText="Resetting…"
        variant="danger"
        className="w-full mt-2"
      >
        Yes, reset my account
      </Button>
      <Button
        data-testid="reset-back-button"
        onClick={onBack}
        variant="ghost"
        disabled={isLoading}
        className="w-full"
      >
        Back
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
}> = ({ heading, body, actionLabel, onAction, onCancel, tone }) => (
  <div className="flex flex-col gap-4">
    <div>
      <h2
        className="text-sm font-mono font-bold"
        style={{ color: tone === "error" ? "#ff6b6b" : "var(--c-text)" }}
      >
        {heading}
      </h2>
      <p
        className="text-xs mt-2 font-mono"
        style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
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
      Sign in as someone else
    </Button>
  </div>
);

// ── Helpers ────────────────────────────────────────────────────────────────

function secondsUntil(rfc3339: string): number {
  const target = new Date(rfc3339).getTime();
  const now = Date.now();
  return Math.max(0, Math.floor((target - now) / 1000));
}

function formatSecondsLeft(s: number): string {
  const m = Math.floor(s / 60);
  const r = s % 60;
  return `${m}:${r.toString().padStart(2, "0")} left`;
}
