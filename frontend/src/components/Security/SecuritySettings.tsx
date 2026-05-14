import React, { useMemo, useState } from "react";
import {
  Download,
  Upload,
  ShieldCheck,
  Lock,
  RefreshCw,
  Eye,
  EyeOff,
} from "lucide-react";
import { Button } from "../ui/Button";

export interface VerifiedContact {
  id: string;
  name: string;
  fingerprint: string;
  verifiedAt: number;
}

export interface SessionInfo {
  contactId: string;
  contactName: string;
  sessionId: string;
  lastRatchetAt: number;
  status: "active" | "stale" | "warning";
}

interface SecuritySettingsProps {
  ownFingerprint: string;
  verifiedContacts?: VerifiedContact[];
  sessions?: SessionInfo[];
  messagePreviewsEnabled?: boolean;
  onToggleMessagePreviews?: (enabled: boolean) => void;
  onExportBackup?: (password: string) => void;
  onImportBackup?: (password: string, file?: File) => void;
  onClearSessions?: () => void;
  onResetSession?: (sessionId: string) => void;
}

const formatFingerprint = (fp: string) => (fp.match(/.{1,5}/g) || []).join(" ");

const strength = (password: string) => {
  let score = 0;
  if (password.length >= 12) score += 1;
  if (/[A-Z]/.test(password)) score += 1;
  if (/[a-z]/.test(password)) score += 1;
  if (/[0-9]/.test(password)) score += 1;
  if (/[^A-Za-z0-9]/.test(password)) score += 1;

  const labels = ["Very weak", "Weak", "Fair", "Good", "Strong", "Excellent"];
  return { score, label: labels[score] };
};

export const SecuritySettings: React.FC<SecuritySettingsProps> = ({
  ownFingerprint,
  verifiedContacts = [],
  sessions = [],
  messagePreviewsEnabled = false,
  onToggleMessagePreviews,
  onExportBackup,
  onImportBackup,
  onClearSessions,
  onResetSession,
}) => {
  const [exportPassword, setExportPassword] = useState("");
  const [importPassword, setImportPassword] = useState("");
  const [importFile, setImportFile] = useState<File | undefined>(undefined);
  const [showExportPassword, setShowExportPassword] = useState(false);
  const [showImportPassword, setShowImportPassword] = useState(false);

  const exportStrength = useMemo(() => strength(exportPassword), [exportPassword]);
  const importStrength = useMemo(() => strength(importPassword), [importPassword]);

  const strengthColor = (score: number) =>
    score <= 1 ? '#ff6b6b' : score <= 2 ? '#f0b429' : score <= 3 ? 'var(--c-accent-dim)' : 'var(--c-accent)';

  return (
    <div
      data-testid="security-settings"
      className="flex flex-col gap-8"
      style={{ background: 'var(--c-bg)' }}
    >
      {/* Safety number */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>
          <span className="inline-flex items-center gap-1.5"><Lock size={17} aria-hidden="true" />Identity</span>
        </h2>
        <div className="flex flex-col gap-0.5">
          <p className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>Your safety number</p>
          <p className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>{formatFingerprint(ownFingerprint)}</p>
        </div>
      </section>

      {/* Message previews */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>Notifications</h2>
        <label className="flex items-center gap-3 cursor-pointer">
          <input
            data-testid="message-previews-toggle"
            type="checkbox"
            checked={messagePreviewsEnabled}
            onChange={(e) => onToggleMessagePreviews?.(e.target.checked)}
            className="w-3 h-3"
            style={{ accentColor: 'var(--c-accent)' }}
          />
          <div className="flex flex-col gap-0.5">
            <span className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>Message previews</span>
            <span className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Show decrypted previews in notifications — may leak on lock screen</span>
          </div>
        </label>
      </section>

      {/* Backup */}
      <section className="flex flex-col gap-4">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>Encrypted Backup</h2>

        <div className="grid grid-cols-2 gap-4">
          {/* Export */}
          <div className="flex flex-col gap-3">
            <h3 className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>Export keys</h3>
            <div className="flex flex-col gap-1.5">
              <label htmlFor="export-password" className="section-label px-0">Password</label>
              <div className="flex gap-1">
                <input
                  id="export-password"
                  data-testid="export-password-input"
                  type={showExportPassword ? "text" : "password"}
                  value={exportPassword}
                  onChange={(e) => setExportPassword(e.target.value)}
                  placeholder="Strong unique password"
                  className="pollis-input flex-1"
                />
                <button
                  type="button"
                  onClick={() => setShowExportPassword((p) => !p)}
                  aria-label="Toggle password visibility"
                  className="icon-btn"
                >
                  {showExportPassword ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
                </button>
              </div>
              {exportPassword && (
                <span className="text-2xs font-mono" style={{ color: strengthColor(exportStrength.score) }}>
                  {exportStrength.label}
                </span>
              )}
            </div>
            <Button
              data-testid="export-backup-button"
              onClick={() => onExportBackup?.(exportPassword)}
              disabled={exportStrength.score < 3}
              variant="primary"
              className="self-start"
            >
              <Download size={17} aria-hidden="true" />
              Export
            </Button>
            <p className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Store in a safe place — losing it means losing access.</p>
          </div>

          {/* Import */}
          <div className="flex flex-col gap-3">
            <h3 className="text-2xs font-mono uppercase tracking-widest" style={{ color: 'var(--c-text-muted)' }}>Import keys</h3>
            <div className="flex flex-col gap-1.5">
              <label htmlFor="import-password" className="section-label px-0">Password</label>
              <div className="flex gap-1">
                <input
                  id="import-password"
                  data-testid="import-password-input"
                  type={showImportPassword ? "text" : "password"}
                  value={importPassword}
                  onChange={(e) => setImportPassword(e.target.value)}
                  placeholder="Password used during export"
                  className="pollis-input flex-1"
                />
                <button
                  type="button"
                  onClick={() => setShowImportPassword((p) => !p)}
                  aria-label="Toggle password visibility"
                  className="icon-btn"
                >
                  {showImportPassword ? <EyeOff size={15} aria-hidden="true" /> : <Eye size={15} aria-hidden="true" />}
                </button>
              </div>
              {importPassword && (
                <span className="text-2xs font-mono" style={{ color: strengthColor(importStrength.score) }}>
                  {importStrength.label}
                </span>
              )}
            </div>
            <input
              data-testid="import-backup-file-input"
              type="file"
              accept=".json,.bak"
              onChange={(e) => setImportFile(e.target.files?.[0])}
              aria-label="Select backup file"
              className="text-xs font-mono"
              style={{ color: 'var(--c-text-dim)' }}
            />
            <Button
              data-testid="import-backup-button"
              onClick={() => onImportBackup?.(importPassword, importFile)}
              disabled={!importFile || importStrength.score < 2}
              variant="primary"
              className="self-start"
            >
              <Upload size={17} aria-hidden="true" />
              Import
            </Button>
            <p className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>Replaces current identity keys. Use cautiously.</p>
          </div>
        </div>
      </section>

      {/* Verified contacts */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>
          Verified Contacts
          <span className="ml-2 font-normal" style={{ color: 'var(--c-text-muted)' }}>{verifiedContacts.length}</span>
        </h2>
        {verifiedContacts.length === 0 ? (
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>No verified contacts yet.</p>
        ) : (
          <div className="flex flex-col divide-y" style={{ borderColor: 'var(--c-border)' }}>
            {verifiedContacts.map((c) => (
              <div key={c.id} data-testid={`verified-contact-${c.id}`} className="flex items-center justify-between py-2 gap-3">
                <div className="flex flex-col gap-0.5 min-w-0">
                  <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>{c.name}</p>
                  <p className="text-2xs font-mono truncate" style={{ color: 'var(--c-text-muted)' }}>{formatFingerprint(c.fingerprint)}</p>
                </div>
                <span className="inline-flex items-center gap-1 text-2xs font-mono flex-shrink-0" style={{ color: 'var(--c-accent)' }}>
                  <ShieldCheck size={19} aria-hidden="true" />
                  verified
                </span>
              </div>
            ))}
          </div>
        )}
      </section>

      {/* Sessions */}
      <section className="flex flex-col gap-3">
        <h2 className="section-label px-0 border-b pb-1" style={{ borderColor: 'var(--c-border)' }}>Sessions</h2>
        {sessions.length === 0 ? (
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-muted)' }}>No active sessions.</p>
        ) : (
          <div className="flex flex-col divide-y" style={{ borderColor: 'var(--c-border)' }}>
            {sessions.map((s) => {
              const statusColor = s.status === 'active' ? 'var(--c-accent)' : s.status === 'warning' ? '#f0b429' : 'var(--c-text-muted)';
              return (
                <div key={s.sessionId} data-testid={`session-${s.sessionId}`} className="flex items-center justify-between py-2 gap-3">
                  <div className="flex flex-col gap-0.5 min-w-0">
                    <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>{s.contactName}</p>
                    <p className="text-2xs font-mono" style={{ color: 'var(--c-text-muted)' }}>
                      {new Date(s.lastRatchetAt).toLocaleString()}
                    </p>
                  </div>
                  <div className="flex items-center gap-2 flex-shrink-0">
                    <span className="text-2xs font-mono" style={{ color: statusColor }}>{s.status}</span>
                    <button
                      data-testid={`reset-session-${s.sessionId}`}
                      onClick={() => onResetSession?.(s.sessionId)}
                      className="icon-btn-sm"
                      aria-label="Reset session"
                    >
                      <RefreshCw size={17} aria-hidden="true" />
                    </button>
                  </div>
                </div>
              );
            })}
          </div>
        )}
        <Button
          data-testid="clear-all-sessions-button"
          onClick={onClearSessions}
          variant="ghost"
          className="self-start"
        >
          Clear all sessions
        </Button>
      </section>
    </div>
  );
};
