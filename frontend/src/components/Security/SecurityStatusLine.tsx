import React from "react";

interface SecurityStatusLineProps {
  testId: string;
  status: string;
  icon: React.ReactElement;
  label: string;
  color: string;
  /** Shown under the label (or after it, with `singleLine`), in the same tone, when non-empty. */
  reason?: string | false;
  /** Keep to one line: the reason follows the label, truncated, in full on hover (#2114). */
  singleLine?: boolean;
  children?: React.ReactNode;
}

/** One advisory check's status on the Security page: icon + label, an optional reason, then any extra controls. */
export const SecurityStatusLine: React.FC<SecurityStatusLineProps> = ({
  testId,
  status,
  icon,
  label,
  color,
  reason,
  singleLine = false,
  children,
}) => (
  <div
    data-testid={testId}
    data-status={status}
    className="flex flex-col gap-1"
  >
    <span
      className={
        singleLine
          ? "flex items-center gap-1 text-meta font-mono min-w-0 whitespace-nowrap"
          : "inline-flex items-center gap-1 text-meta font-mono"
      }
      style={{ color }}
      title={singleLine && reason ? `${label}: ${reason}` : undefined}
    >
      <span className="shrink-0 inline-flex">{icon}</span>
      <span className={singleLine ? "shrink-0" : undefined}>{label}</span>
      {singleLine && reason && (
        <span data-testid={`${testId}-reason`} className="min-w-0 truncate">
          {`· ${reason}`}
        </span>
      )}
    </span>
    {!singleLine && reason && (
      <span
        data-testid={`${testId}-reason`}
        className="text-meta font-mono"
        style={{ color }}
      >
        {reason}
      </span>
    )}
    {children}
  </div>
);
