import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion } from "lucide-react";
import { shellOpen } from "../../bridge";
import type { BuildVerifyStatus } from "../../types";

interface BuildVerifyLineProps {
  status: BuildVerifyStatus;
  // Shown as the reason on `mismatch` and `unavailable`.
  detail: string;
  testId?: string;
}

// The in-app check proves inclusion + hash match, not reproducibility; that's for rebuilders.
const VERIFY_DOCS_URL =
  "https://pollis.com/artifacts.html";

// Advisory, never blocking; only `mismatch` is danger-toned. `labelKey` resolves at render.
const statusConfig: Record<
  BuildVerifyStatus,
  { icon: React.ReactElement; labelKey: string; color: string }
> = {
  verified: {
    icon: <ShieldCheck size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusVerified",
    color: "var(--c-accent)",
  },
  pending: {
    icon: <Clock size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusPending",
    color: "var(--c-text-muted)",
  },
  mismatch: {
    icon: <AlertTriangle size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusMismatch",
    color: "var(--c-danger)",
  },
  unavailable: {
    icon: <ShieldQuestion size={14} aria-hidden="true" />,
    labelKey: "security.buildStatusUnavailable",
    color: "var(--c-text-muted)",
  },
};

// "Verify this build" status (#484); the guide link shows on `unavailable` too, since some installs can't self-verify.
export const BuildVerifyLine: React.FC<BuildVerifyLineProps> = ({
  status,
  detail,
  testId = "build-verify",
}) => {
  const { t } = useTranslation("settings");
  const { icon, labelKey, color } = statusConfig[status];
  const showReason = status === "mismatch" || status === "unavailable";

  return (
    <div
      data-testid={testId}
      data-status={status}
      className="flex flex-col gap-1"
    >
      <span
        className="inline-flex items-center gap-1 text-2xs font-mono"
        style={{ color }}
      >
        {icon}
        {t(labelKey)}
      </span>
      {/* Reason plus the verify-guide link. */}
      {showReason && (
        <>
          {detail && (
            <span
              data-testid={`${testId}-reason`}
              className="text-2xs font-mono"
              style={{ color }}
            >
              {detail}
            </span>
          )}
          <button
            type="button"
            data-testid={`${testId}-learn-more`}
            className="text-2xs font-mono underline self-start"
            style={{ color }}
            onClick={() => {
              void shellOpen(VERIFY_DOCS_URL);
            }}
          >
            {t("security.buildVerifyGuideLink")}
          </button>
        </>
      )}
    </div>
  );
};
