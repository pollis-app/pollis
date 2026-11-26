import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion, ShieldEllipsis } from "lucide-react";
import { ACCOUNT_KEY_LABEL_KEYS, type AccountKeyDisplayStatus } from "./accountKeyAuditStatus";
import { SecurityStatusLine } from "./SecurityStatusLine";

interface AccountKeyAuditLineProps {
  /** From `accountKeyDisplayStatus`: `checking` while the audit runs, then its verdict. */
  status: AccountKeyDisplayStatus;
  // Shown as the reason on `alarm` only, on the same line.
  detail: string;
  testId?: string;
}

// Advisory, never blocking; only `alarm` is warning-toned. Labels resolve at render (module-level `t()` would freeze the language).
const statusConfig: Record<
  AccountKeyDisplayStatus,
  { icon: React.ReactElement; color: string }
> = {
  checking: {
    icon: <ShieldEllipsis size={14} aria-hidden="true" />,
    color: "var(--c-text-muted)",
  },
  ok: {
    icon: <ShieldCheck size={14} aria-hidden="true" />,
    color: "var(--c-accent)",
  },
  pending: {
    icon: <Clock size={14} aria-hidden="true" />,
    color: "var(--c-text-muted)",
  },
  alarm: {
    icon: <AlertTriangle size={14} aria-hidden="true" />,
    color: "var(--c-warning)",
  },
  unavailable: {
    icon: <ShieldQuestion size={14} aria-hidden="true" />,
    color: "var(--c-text-muted)",
  },
};

// Account-key transparency audit status (#330). One line in every state, so nothing below it moves (#2114).
export const AccountKeyAuditLine: React.FC<AccountKeyAuditLineProps> = ({
  status,
  detail,
  testId = "account-key-audit",
}) => {
  const { t } = useTranslation("settings");
  const { icon, color } = statusConfig[status];

  return (
    <SecurityStatusLine
      testId={testId}
      status={status}
      icon={icon}
      label={t(ACCOUNT_KEY_LABEL_KEYS[status])}
      color={color}
      reason={status === "alarm" && detail}
      singleLine
    />
  );
};
