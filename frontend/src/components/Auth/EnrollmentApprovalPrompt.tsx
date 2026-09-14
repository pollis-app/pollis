import { errorMessage } from "../../utils/errorMessage";
import React, { useMemo, useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { TitleBar } from "../Layout/TitleBar";
import { DotMatrix } from "../ui/DotMatrix";
import { Card } from "../ui/Card";
import { Button } from "../ui/Button";
import { TextInput } from "../ui/TextInput";
import { SAS_LENGTH, normalizeSasInput } from "../../utils/enrollmentSas";
import * as api from "../../services/api";

interface EnrollmentApprovalPromptProps {
  requestId: string;
  newDeviceId: string;
  onResolved: () => void;
}

/// Full-screen takeover shown on every existing device when a sibling
/// device of the same user posts a `device_enrollment_request`. The user
/// must explicitly approve or reject — there is no auto-dismiss because
/// silently ignoring an enrollment request would be a quiet account
/// takeover vector.
///
/// The approver TYPES the code shown on the new device (#1096). This screen
/// used to display the server's copy of it and submit that same value back on
/// one click, so the SAS comparison the whole flow rests on was performed by
/// nobody: a Delivery Service that swapped the new device's ephemeral key and
/// its stored code to match satisfied every programmatic check, and approving
/// handed the account private key to whoever controlled the substituted key.
/// Typing puts the code the USER read off the other screen on one side of the
/// comparison, and the code Rust derives from the ephemeral key it fetched on
/// the other — so a swap fails without depending on the user's diligence.
export const EnrollmentApprovalPrompt: React.FC<EnrollmentApprovalPromptProps> = ({
  requestId,
  newDeviceId,
  onResolved,
}) => {
  const { t } = useTranslation("auth");
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [typedCode, setTypedCode] = useState("");

  const isComplete = typedCode.length === SAS_LENGTH;

  const handleApprove = async () => {
    if (!isComplete || isLoading) {
      return;
    }
    setIsLoading(true);
    setError(null);
    try {
      await api.approveDeviceEnrollment(requestId, typedCode);
      onResolved();
    } catch (err) {
      setError(errorMessage(err, t("approval.approveFailed")));
      // Clear on failure: a wrong code is the signal to go and re-read the
      // other screen, not to nudge one character and retry.
      setTypedCode("");
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
      setError(errorMessage(err, t("approval.rejectFailed")));
    } finally {
      setIsLoading(false);
    }
  };

  // Truncate the device id for display so the prompt is readable.
  const shortDeviceId = useMemo(
    () => `${newDeviceId.slice(0, 6)}…${newDeviceId.slice(-4)}`,
    [newDeviceId],
  );

  return (
    <div
      data-testid="enrollment-approval-prompt"
      className="flex flex-col h-full w-full bg-bg"
      style={{
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
        className="flex-1 flex justify-center overflow-y-auto"
        style={{ position: "relative", zIndex: 1, padding: "1rem" }}
      >
        <Card
          padding="lg"
          className="my-auto"
          style={{
            width: "100%",
            maxWidth: 480,
            border: "2px solid var(--c-danger)",
          }}
        >
          <div className="flex flex-col gap-5">
            <div>
              <p
                className="text-xs font-mono uppercase tracking-wider text-danger"
                style={{ letterSpacing: "0.15em" }}
              >
                {t("approval.badge")}
              </p>
              <h1 className="text-base font-mono font-bold mt-1 text-fg">
                {t("approval.title")}
              </h1>
              <p
                className="text-xs mt-2 font-mono text-fg"
                style={{ lineHeight: 1.6 }}
              >
                <Trans
                  t={t}
                  i18nKey="approval.body"
                  values={{ deviceId: shortDeviceId }}
                  components={{ code: <code /> }}
                />
              </p>
              <p
                className="text-xs mt-2 font-mono text-muted"
                style={{ lineHeight: 1.6 }}
              >
                {t("approval.instruction")}
              </p>
            </div>

            <TextInput
              data-testid="approval-code-input"
              label={t("approval.codeLabel")}
              description={t("approval.codeHint", { count: SAS_LENGTH })}
              value={typedCode}
              onChange={(next) => {
                setError(null);
                setTypedCode(normalizeSasInput(next));
              }}
              placeholder={"·".repeat(SAS_LENGTH)}
              disabled={isLoading}
              autoFocus
              autoComplete="off"
              className="font-mono"
            />

            {error && (
              <p
                data-testid="approval-error"
                className="text-xs font-mono text-danger"
              >
                {error}
              </p>
            )}

            <div className="flex flex-col gap-2">
              <Button
                data-testid="approve-enrollment-button"
                onClick={handleApprove}
                isLoading={isLoading}
                loadingText={t("approval.approving")}
                disabled={!isComplete}
                className="w-full"
              >
                {t("approval.approve")}
              </Button>
              <Button
                data-testid="reject-enrollment-button"
                onClick={handleReject}
                disabled={isLoading}
                variant="danger"
                className="w-full"
              >
                {t("approval.reject")}
              </Button>
            </div>
          </div>
        </Card>
      </div>
    </div>
  );
};
