import React from "react";
import {
  Lock,
  ShieldCheck,
  ShieldAlert,
  AlertTriangle,
  Shield,
} from "lucide-react";

type IndicatorKind =
  | "encrypted"
  | "verified"
  | "warning"
  | "unverified"
  | "group-shield";

interface SecurityIndicatorProps {
  kind: IndicatorKind;
  label?: string;
}

const kindConfig: Record<IndicatorKind, { icon: React.ReactElement; text: string; color: string }> = {
  encrypted: { icon: <Lock size={14} aria-hidden="true" />, text: "Encrypted", color: 'var(--c-accent)' },
  verified: { icon: <ShieldCheck size={14} aria-hidden="true" />, text: "Verified", color: 'var(--c-accent)' },
  warning: { icon: <AlertTriangle size={14} aria-hidden="true" />, text: "Warning", color: '#f0b429' },
  unverified: { icon: <ShieldAlert size={14} aria-hidden="true" />, text: "Unverified", color: 'var(--c-danger)' },
  "group-shield": { icon: <Shield size={14} aria-hidden="true" />, text: "Group protected", color: 'var(--c-accent-dim)' },
};

export const SecurityIndicator: React.FC<SecurityIndicatorProps> = ({
  kind,
  label,
}) => {
  const { icon, text, color } = kindConfig[kind];

  return (
    <span
      data-testid="security-indicator"
      className="inline-flex items-center gap-1 text-2xs font-mono"
      style={{ color }}
    >
      {icon}
      {label || text}
    </span>
  );
};
