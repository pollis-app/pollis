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

/// Full-screen takeover on every existing device when a sibling device requests
/// enrollment. No auto-dismiss: ignoring a request silently would be a quiet
/// takeover vector.
///
/// The approver TYPES the code shown on the new device (#1096), so the SAS
/// comparison is the user's reading against the code Rust derives from the
/// fetched ephemeral key, and a DS-side key swap fails.
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
      // Clear on failure: the user should re-read the other screen.
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
