import React, { useState } from "react";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import * as api from "../../services/api";

interface EnrollmentApprovalPromptProps {
  requestId: string;
  newDeviceId: string;
  verificationCode: string;
  onResolved: () => void;
}

/// Full-screen takeover shown on every existing device when a sibling
/// device of the same user posts a `device_enrollment_request`. The user
/// must explicitly approve or reject — there is no auto-dismiss because
/// silently ignoring an enrollment request would be a quiet account
/// takeover vector.
export const EnrollmentApprovalPrompt: React.FC<EnrollmentApprovalPromptProps> = ({
  requestId,
  newDeviceId,
  verificationCode,
  onResolved,
}) => {
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleApprove = async () => {
    setIsLoading(true);
    setError(null);
    try {
      await api.approveDeviceEnrollment(requestId, verificationCode);
      onResolved();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to approve");
    } finally {
      setIsLoading(false);
    }
  };

  const handleReject = async () => {
    setIsLoading(true);
    setError(null);
    try {
      await api.rejectDeviceEnrollment(requestId);
      onResolved();
    } catch (err) {
      setError(err instanceof Error ? err.message : "Failed to reject");
    } finally {
      setIsLoading(false);
    }
  };

  // Truncate the device id for display so the prompt is readable.
  const shortDeviceId = `${newDeviceId.slice(0, 6)}…${newDeviceId.slice(-4)}`;

  return (
    <div
      data-testid="enrollment-approval-prompt"
      className="flex flex-col h-full w-full"
      style={{
        background: "var(--c-bg)",
        position: "fixed",
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        zIndex: 9999,
      }}
    >
      <div style={{ position: "absolute", inset: 0, opacity: 0.45, pointerEvents: "none" }}>
        <DotMatrix speed={1.6} />
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
            maxWidth: 480,
            border: "2px solid #ff6b6b",
          }}
        >
          <div className="flex flex-col gap-5">
            <div>
              <p
                className="text-xs font-mono uppercase tracking-wider"
                style={{ color: "#ff6b6b", letterSpacing: "0.15em" }}
              >
                ⚠ Security request
              </p>
              <h1
                className="text-base font-mono font-bold mt-1"
                style={{ color: "var(--c-text)" }}
              >
                A new device wants to enroll
              </h1>
              <p
                className="text-xs mt-2 font-mono"
                style={{ color: "var(--c-text)", lineHeight: 1.6 }}
              >
                A device claiming to be yours (id <code>{shortDeviceId}</code>)
                just signed in with your email and is asking to be added to
                your account.
              </p>
              <p
                className="text-xs mt-2 font-mono"
                style={{ color: "var(--c-text-muted)", lineHeight: 1.6 }}
              >
                Only approve if you started this on another device just now.
                The 6-digit code below MUST match the code shown on the new
                device. If it doesn't match, reject this request.
              </p>
            </div>

            <div
              data-testid="approval-verification-code"
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
              {verificationCode}
            </div>

            {error && (
              <p
                data-testid="approval-error"
                className="text-xs font-mono"
                style={{ color: "#ff6b6b" }}
              >
                {error}
              </p>
            )}

            <div className="flex flex-col gap-2">
              <Button
                data-testid="approve-enrollment-button"
                onClick={handleApprove}
                isLoading={isLoading}
                loadingText="Approving…"
                className="w-full"
              >
                Yes, this is me — approve
              </Button>
              <Button
                data-testid="reject-enrollment-button"
                onClick={handleReject}
                disabled={isLoading}
                variant="danger"
                className="w-full"
              >
                Not me — reject
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
