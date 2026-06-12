import React from "react";
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
const statusConfig: Record<
  AuditStatus,
  { icon: React.ReactElement; label: string; color: string }
> = {
  ok: {
    icon: <ShieldCheck size={14} aria-hidden="true" />,
    label: "Key publicly verified",
    color: "var(--c-accent)",
  },
  pending: {
    icon: <Clock size={14} aria-hidden="true" />,
    label: "Key publication pending",
    color: "var(--c-text-muted)",
  },
  alarm: {
    icon: <AlertTriangle size={14} aria-hidden="true" />,
    label: "Key verification failed",
    color: "#f0b429",
  },
  unavailable: {
    icon: <ShieldQuestion size={14} aria-hidden="true" />,
    label: "Verification unavailable",
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
  const { icon, label, color } = statusConfig[status];

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
        {label}
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
