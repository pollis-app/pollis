import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion } from "lucide-react";
import type { AuditStatus } from "../../types";
import { SecurityStatusLine } from "./SecurityStatusLine";

interface AccountKeyAuditLineProps {
  status: AuditStatus;
  // Shown as the reason on `alarm` only.
  detail: string;
  testId?: string;
}

// Advisory, never blocking; only `alarm` is warning-toned. `labelKey` resolves at render (module-level `t()` would freeze the language).
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

// Account-key transparency audit status (#330).
export const AccountKeyAuditLine: React.FC<AccountKeyAuditLineProps> = ({
  status,
  detail,
  testId = "account-key-audit",
}) => {
  const { t } = useTranslation("settings");
  const { icon, labelKey, color } = statusConfig[status];

  return (
    <SecurityStatusLine
      testId={testId}
      status={status}
      icon={icon}
      label={t(labelKey)}
      color={color}
      reason={status === "alarm" && detail}
    />
  );
};
