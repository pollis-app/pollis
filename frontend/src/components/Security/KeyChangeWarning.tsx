import React from "react";
import { ShieldAlert, ShieldCheck, XCircle } from "lucide-react";
import { Button } from "../ui/Button";

interface KeyChangeWarningProps {
  contactName: string;
  oldFingerprint?: string;
  newFingerprint: string;
  onReverify?: () => void;
  onContinue?: () => void;
  onCancel?: () => void;
}

const formatFingerprint = (fp?: string) =>
  fp ? (fp.match(/.{1,5}/g) || []).join(" ") : "Unknown";

export const KeyChangeWarning: React.FC<KeyChangeWarningProps> = ({
  contactName,
  oldFingerprint,
  newFingerprint,
  onReverify,
  onContinue,
  onCancel,
}) => {
  return (
    <div
      data-testid="key-change-warning"
      className="flex flex-col gap-4 p-4 rounded-panel"
      style={{ border: '1px solid #f0b429', background: 'rgba(240,180,41,0.06)' }}
    >
      <div className="flex items-start gap-3">
        <ShieldAlert size={19} aria-hidden="true" style={{ color: '#f0b429', flexShrink: 0, marginTop: 2 }} />
        <div className="flex flex-col gap-1">
          <h3 className="text-sm font-mono font-medium" style={{ color: '#f0b429' }}>
            Identity key changed — {contactName}
          </h3>
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
            This could mean a reinstall or new device. Verify the new safety number with {contactName} before continuing.
          </p>
        </div>
      </div>

      <div className="flex flex-col gap-2 pl-7">
        {oldFingerprint && (
          <div className="flex flex-col gap-0.5">
            <p className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>Previous</p>
            <p className="text-xs font-mono line-through" style={{ color: 'var(--c-text-muted)' }}>{formatFingerprint(oldFingerprint)}</p>
          </div>
        )}
        <div className="flex flex-col gap-0.5">
          <p className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>New</p>
          <p className="text-xs font-mono" style={{ color: '#f0b429' }}>{formatFingerprint(newFingerprint)}</p>
        </div>
      </div>

      <div className="flex items-center gap-2 pl-7">
        <Button data-testid="reverify-button" onClick={onReverify} variant="primary">
          <ShieldCheck size={17} aria-hidden="true" />
          Re-verify
        </Button>
        <Button data-testid="continue-anyway-button" onClick={onContinue} variant="ghost">
          Continue anyway
        </Button>
        <button data-testid="cancel-key-warning-button" onClick={onCancel} className="icon-btn-sm ml-auto">
          <XCircle size={19} aria-hidden="true" />
        </button>
      </div>
    </div>
  );
};
