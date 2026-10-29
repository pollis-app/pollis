import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion } from "lucide-react";
import type { AuditStatus } from "../../types";

interface AccountKeyAuditLineProps {
  status: AuditStatus;
  // Shown as the reason on `alarm` only.
  detail: string;
  testId?: string;
}

// Copy + tone per audit status: advisory, never blocking; only `alarm` is
// warning-toned. `labelKey` is resolved at render, not import.
const statusConfig: Record<
  AuditStatus,
  { icon: React.ReactElement; labelKey: string; color: string }
> = {
  ok: {
    icon: <ShieldCheck size={14} aria-hidden="true" />,
    labelKey: "security.accountKeyStatusOk",
    color: "var(--c-accent)",
  },
  pending: {
    icon: <Clock size={14} aria-hidden="true" />,
    labelKey: "security.accountKeyStatusPending",
    color: "var(--c-text-muted)",
  },
  alarm: {
    icon: <AlertTriangle size={14} aria-hidden="true" />,
    labelKey: "security.accountKeyStatusAlarm",
    color: "#f0b429",
  },
  unavailable: {
    icon: <ShieldQuestion size={14} aria-hidden="true" />,
    labelKey: "security.accountKeyStatusUnavailable",
    color: "var(--c-text-muted)",
  },
};

// Advisory account-key transparency audit status (#330), for peer and self.
export const AccountKeyAuditLine: React.FC<AccountKeyAuditLineProps> = ({
  status,
  detail,
  testId = "account-key-audit",
}) => {
  const { t } = useTranslation("settings");
  const { icon, labelKey, color } = statusConfig[status];

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
      {status === "alarm" && detail && (
        <span
          data-testid={`${testId}-reason`}
          className="text-2xs font-mono"
          style={{ color }}
        >
          {detail}
        </span>
      )}
    </div>
  );
};
