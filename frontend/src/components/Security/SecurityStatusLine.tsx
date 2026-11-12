import React from "react";

interface SecurityStatusLineProps {
  testId: string;
  status: string;
  icon: React.ReactElement;
  label: string;
  color: string;
  /** Shown under the label, in the same tone, when non-empty. */
  reason?: string | false;
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
  children,
}) => (
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
    {reason && (
      <span
        data-testid={`${testId}-reason`}
        className="text-2xs font-mono"
        style={{ color }}
      >
        {reason}
      </span>
    )}
    {children}
  </div>
);
