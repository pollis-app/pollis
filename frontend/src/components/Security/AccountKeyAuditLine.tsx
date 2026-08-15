import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldCheck, Clock, AlertTriangle, ShieldQuestion } from "lucide-react";
import type { AuditStatus } from "../../types";

interface AccountKeyAuditLineProps {
  status: AuditStatus;
  // One-line, human-readable explanation from the audit report. Surfaced as
  // the reason on `alarm`; ignored for the other (self-explanatory) statuses.
  detail: string;
  testId?: string;
}

// Terse copy + tone per audit status. Quiet and advisory — these alert, they
// never block. `alarm` is the only warning-toned case (amber, matching
// SecurityIndicator's `warning`); the rest sit in muted/accent text.
//
// `labelKey` is a `settings` translation key rather than the copy itself: this
// table is module-level, so a `t()` here would snapshot the language at import
// time. It is resolved in the component below, on every render.
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

// A small, advisory status line surfacing the result of an account-key
// transparency audit (issue #330). Used on the peer profile (peer audit) and
// the security page (self audit).
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
      {/* On alarm, append the report's reason so the user can act on it. */}
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
