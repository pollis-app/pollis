import React, { useMemo, useState } from "react";
import { QRCodeSVG } from "qrcode.react";
import { Check, Copy, ShieldAlert, ShieldCheck, X } from "lucide-react";
import { Button } from "../ui/Button";

interface KeyVerificationProps {
  contactName: string;
  contactId?: string;
  localFingerprint: string;
  remoteFingerprint: string;
  keyChanged?: boolean;
  onVerified?: (contactId?: string) => void;
  onCancel?: () => void;
}

const formatSafetyNumber = (fingerprint: string): string[] => {
  return (
    fingerprint
      .replace(/[^a-zA-Z0-9]/g, "")
      .toUpperCase()
      .match(/.{1,5}/g) || []
  );
};

export const KeyVerification: React.FC<KeyVerificationProps> = ({
  contactName,
  contactId,
  localFingerprint,
  remoteFingerprint,
  keyChanged = false,
  onVerified,
  onCancel,
}) => {
  const [manualEntry, setManualEntry] = useState("");
  const [copied, setCopied] = useState(false);

  const remoteGroups = useMemo(
    () => formatSafetyNumber(remoteFingerprint),
    [remoteFingerprint]
  );
  const localGroups = useMemo(
    () => formatSafetyNumber(localFingerprint),
    [localFingerprint]
  );

  const matches =
    formatSafetyNumber(manualEntry).join("") ===
    formatSafetyNumber(remoteFingerprint).join("");

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(remoteFingerprint);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch (error) {
      console.error("Failed to copy fingerprint", error);
    }
  };

  return (
    <div
      data-testid="key-verification"
      className="flex flex-col gap-6 p-6"
      style={{ background: 'var(--c-bg)' }}
    >
      {/* Header */}
      <div className="flex items-start justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h1 className="text-sm font-mono font-medium" style={{ color: 'var(--c-accent)' }}>
            Verify {contactName}
          </h1>
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
            Compare safety numbers or scan QR to verify identity.
          </p>
        </div>
        {keyChanged && (
          <span className="inline-flex items-center gap-1 text-2xs font-mono flex-shrink-0" style={{ color: '#f0b429' }}>
            <ShieldAlert size={19} aria-hidden="true" />
            Key changed
          </span>
        )}
      </div>

      {/* Safety numbers side-by-side */}
      <div className="grid grid-cols-2 gap-4">
        <div className="flex flex-col gap-2">
          <h3 className="section-label px-0">You</h3>
          <div className="flex flex-wrap gap-1">
            {localGroups.map((g, idx) => (
              <span key={`local-${idx}`} className="text-xs font-mono px-1 rounded" style={{ background: 'var(--c-surface-high)', color: 'var(--c-text-dim)' }}>{g}</span>
            ))}
          </div>
        </div>

        <div className="flex flex-col gap-2">
          <h3 className="section-label px-0">
            {contactName}
            {keyChanged && <span className="ml-2 text-2xs" style={{ color: '#f0b429' }}>needs verification</span>}
          </h3>
          <div className="flex flex-wrap gap-1">
            {remoteGroups.map((g, idx) => (
              <span key={`remote-${idx}`} className="text-xs font-mono px-1 rounded" style={{ background: 'var(--c-surface-high)', color: keyChanged ? '#f0b429' : 'var(--c-accent-dim)' }}>{g}</span>
            ))}
          </div>
        </div>
      </div>

      {/* QR + manual */}
      <div className="flex flex-col gap-4">
        <div className="flex items-start gap-4">
          <div className="flex flex-col items-center gap-2 flex-shrink-0">
            <QRCodeSVG
              value={`pollis-verification:${remoteFingerprint}`}
              size={120}
              bgColor="#070908"
              fgColor="#39D98A"
              includeMargin
            />
            <p className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>scan out-of-band</p>
          </div>

          <div className="flex flex-col gap-3 flex-1">
            <div className="flex flex-col gap-1.5">
              <label htmlFor="manual-entry" className="section-label px-0">Manual verification</label>
              <input
                id="manual-entry"
                data-testid="manual-safety-number-input"
                type="text"
                value={manualEntry}
                onChange={(e) => setManualEntry(e.target.value)}
                placeholder="ABCDE FGHIJ …"
                className="pollis-input font-mono"
              />
            </div>

            {manualEntry && (
              <span
                className="inline-flex items-center gap-1 text-xs font-mono"
                style={{ color: matches ? 'var(--c-accent)' : '#ff6b6b' }}
              >
                {matches ? (
                  <><ShieldCheck size={17} data-testid="safety-number-match" aria-hidden="true" /> Matches</>
                ) : (
                  <><X size={17} data-testid="safety-number-no-match" aria-hidden="true" /> No match</>
                )}
              </span>
            )}
          </div>
        </div>

        <Button
          data-testid="copy-safety-number-button"
          onClick={handleCopy}
          aria-label="Copy safety number"
          variant="ghost"
          className="self-start"
        >
          {copied ? <Check size={17} aria-hidden="true" /> : <Copy size={17} aria-hidden="true" />}
          {copied ? "Copied" : "Copy safety number"}
        </Button>
      </div>

      {/* Actions */}
      <div className="flex items-center gap-2">
        <Button
          data-testid="mark-verified-button"
          onClick={() => onVerified?.(contactId)}
          disabled={!matches}
          variant="primary"
        >
          <ShieldCheck size={17} aria-hidden="true" />
          Mark as verified
        </Button>
        <Button
          data-testid="cancel-verification-button"
          onClick={onCancel}
          variant="ghost"
        >
          Cancel
        </Button>
      </div>
    </div>
  );
};
